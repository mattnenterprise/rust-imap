use std::net::{TcpStream, ToSocketAddrs};
use openssl::ssl::{SslContext, SslStream};
use std::io::{Read, Write};

use super::mailbox::Mailbox;
use super::authenticator::Authenticator;
use super::parse::{parse_response_ok, parse_capability, parse_select_or_examine, parse_response, parse_authenticate_response};
use super::error::{Error, Result};

static TAG_PREFIX: &'static str = "a";
const INITIAL_TAG: u32 = 0;
const CR: u8 = 0x0d;
const LF: u8 = 0x0a;

/// Stream to interface with the IMAP server. This interface is only for the command stream.
pub struct Client<T> {
	stream: T,
	tag: u32,
	pub debug: bool
}

impl Client<TcpStream> {
	/// Creates a new client.
	pub fn connect<A: ToSocketAddrs>(addr: A) -> Result<Client<TcpStream>> {
		match TcpStream::connect(addr) {
			Ok(stream) => {
				let mut socket = Client::new(stream);

				try!(socket.read_greeting());
				Ok(socket)
			},
			Err(e) => Err(Error::Io(e))
		}
	}

	/// This will upgrade a regular TCP connection to use SSL.
	pub fn secure(mut self, ssl_context: SslContext) -> Result<Client<SslStream<TcpStream>>> {
		// TODO This needs to be tested
		try!(self.run_command_and_check_ok("STARTTLS"));
		SslStream::connect(&ssl_context, self.stream)
			.map(|s| Client::new(s))
			.map_err(|e| Error::Ssl(e))
	}
}

impl Client<SslStream<TcpStream>> {
	/// Creates a client with an SSL wrapper.
	pub fn secure_connect<A: ToSocketAddrs>(addr: A, ssl_context: SslContext) -> Result<Client<SslStream<TcpStream>>> {
		match TcpStream::connect(addr) {
			Ok(stream) => {
				let ssl_stream = match SslStream::connect(&ssl_context, stream) {
					Ok(s) => s,
					Err(e) => return Err(Error::Ssl(e))
				};
				let mut socket = Client::new(ssl_stream);

				try!(socket.read_greeting());
				Ok(socket)
			},
			Err(e) => Err(Error::Io(e))
		}
	}
}

impl<T: Read+Write> Client<T> {

	/// Creates a new client with the underlying stream.
	pub fn new(stream: T) -> Client<T> {
		Client{
			stream: stream,
			tag: INITIAL_TAG,
			debug: false
		}
	}

	/// Authenticate will authenticate with the server, using the authenticator given.
	pub fn authenticate<A: Authenticator>(&mut self, auth_type: &str, authenticator: A) -> Result<()> {
		try!(self.run_command(&format!("AUTHENTICATE {}", auth_type).to_string()));
		self.do_auth_handshake(authenticator)
	}

	/// This func does the handshake process once the authenticate command is made.
	fn do_auth_handshake<A: Authenticator>(&mut self, authenticator: A) -> Result<()> {
		// TODO Clean up this code
		loop {
			let line = try!(self.readline());

			if line.starts_with(b"+") {
				let data = try!(parse_authenticate_response(String::from_utf8(line).unwrap()));
				let auth_response = authenticator.process(data);

				try!(self.write_line(auth_response.into_bytes().as_slice()))
			} else if line.starts_with(format!("{}{} ", TAG_PREFIX, self.tag).as_bytes()) {
				try!(parse_response(vec![String::from_utf8(line).unwrap()]));
				return Ok(());

			} else {
				let mut lines = try!(self.read_response());
				lines.insert(0, String::from_utf8(line).unwrap());
				try!(parse_response(lines.clone()));
				return Ok(());
			}
		}
	}

	/// Log in to the IMAP server.
	pub fn login(&mut self, username: & str, password: & str) -> Result<()> {
		self.run_command_and_check_ok(&format!("LOGIN {} {}", username, password).to_string())
	}

	/// Selects a mailbox
	pub fn select(&mut self, mailbox_name: &str) -> Result<Mailbox> {
		let lines = try!(
			self.run_command_and_read_response(&format!("SELECT {}", mailbox_name).to_string())
		);
		parse_select_or_examine(lines)
	}

	/// Examine is identical to Select, but the selected mailbox is identified as read-only
	pub fn examine(&mut self, mailbox_name: &str) -> Result<Mailbox> {
		let lines = try!(
			self.run_command_and_read_response(&format!("EXAMINE {}", mailbox_name).to_string())
		);
		parse_select_or_examine(lines)
	}

	/// Fetch retreives data associated with a message in the mailbox.
	pub fn fetch(&mut self, sequence_set: &str, query: &str) -> Result<Vec<String>> {
		self.run_command_and_read_response(&format!("FETCH {} {}", sequence_set, query).to_string())
	}

	pub fn uid_fetch(&mut self, uid_set: &str, query: &str) -> Result<Vec<String>> {
		self.run_command_and_read_response(&format!("UID FETCH {} {}", uid_set, query).to_string())
	}

	/// Noop always succeeds, and it does nothing.
	pub fn noop(&mut self) -> Result<()> {
		self.run_command_and_check_ok("NOOP")
	}

	/// Logout informs the server that the client is done with the connection.
	pub fn logout(&mut self) -> Result<()> {
		self.run_command_and_check_ok("LOGOUT")
	}

	/// Create creates a mailbox with the given name.
	pub fn create(&mut self, mailbox_name: &str) -> Result<()> {
		self.run_command_and_check_ok(&format!("CREATE {}", mailbox_name).to_string())
	}

	/// Delete permanently removes the mailbox with the given name.
	pub fn delete(&mut self, mailbox_name: &str) -> Result<()> {
		self.run_command_and_check_ok(&format!("DELETE {}", mailbox_name).to_string())
	}

	/// Rename changes the name of a mailbox.
	pub fn rename(&mut self, current_mailbox_name: &str, new_mailbox_name: &str) -> Result<()> {
		self.run_command_and_check_ok(&format!("RENAME {} {}", current_mailbox_name, new_mailbox_name).to_string())
	}

	/// Subscribe adds the specified mailbox name to the server's set of "active" or "subscribed"
	/// mailboxes as returned by the LSUB command.
	pub fn subscribe(&mut self, mailbox: &str) -> Result<()> {
		self.run_command_and_check_ok(&format!("SUBSCRIBE {}", mailbox).to_string())
	}

	/// Unsubscribe removes the specified mailbox name from the server's set of "active" or "subscribed"
	/// mailboxes as returned by the LSUB command.
	pub fn unsubscribe(&mut self, mailbox: &str) -> Result<()> {
		self.run_command_and_check_ok(&format!("UNSUBSCRIBE {}", mailbox).to_string())
	}

	/// Capability requests a listing of capabilities that the server supports.
	pub fn capability(&mut self) -> Result<Vec<String>> {
		let lines = try!(
			self.run_command_and_read_response(&format!("CAPABILITY").to_string())
		);
		parse_capability(lines)
	}

	/// Expunge permanently removes all messages that have the \Deleted flag set from the currently
	/// selected mailbox.
	pub fn expunge(&mut self) -> Result<()> {
		self.run_command_and_check_ok("EXPUNGE")
	}

	/// Check requests a checkpoint of the currently selected mailbox.
	pub fn check(&mut self) -> Result<()> {
		self.run_command_and_check_ok("CHECK")
	}

	/// Close permanently removes all messages that have the \Deleted flag set from the currently
	/// selected mailbox, and returns to the authenticated state from the selected state.
	pub fn close(&mut self) -> Result<()> {
		self.run_command_and_check_ok("CLOSE")
	}

	/// Store alters data associated with a message in the mailbox.
	pub fn store(&mut self, sequence_set: &str, query: &str) -> Result<Vec<String>> {
		self.run_command_and_read_response(&format!("STORE {} {}", sequence_set, query))
	}

	pub fn uid_store(&mut self, uid_set: &str, query: &str) -> Result<Vec<String>> {
		self.run_command_and_read_response(&format!("UID STORE {} {}", uid_set, query))
	}

	/// Copy copies the specified message to the end of the specified destination mailbox.
	pub fn copy(&mut self, sequence_set: &str, mailbox_name: &str) -> Result<()> {
		self.run_command_and_check_ok(&format!("COPY {} {}", sequence_set, mailbox_name).to_string())
	}

	pub fn uid_copy(&mut self, uid_set: &str, mailbox_name: &str) -> Result<()> {
		self.run_command_and_check_ok(&format!("UID COPY {} {}", uid_set, mailbox_name))
	}

	/// The LIST command returns a subset of names from the complete set
	/// of all names available to the client.
	pub fn list(&mut self, reference_name: &str, mailbox_search_pattern: &str) -> Result<Vec<String>> {
		self.run_command_and_parse(&format!("LIST {} {}", reference_name, mailbox_search_pattern))
	}

	/// The LSUB command returns a subset of names from the set of names
	/// that the user has declared as being "active" or "subscribed".
	pub fn lsub(&mut self, reference_name: &str, mailbox_search_pattern: &str) -> Result<Vec<String>> {
		self.run_command_and_parse(&format!("LSUB {} {}", reference_name, mailbox_search_pattern))
	}

	/// The STATUS command requests the status of the indicated mailbox.
	pub fn status(&mut self, mailbox_name: &str, status_data_items: &str) -> Result<Vec<String>> {
		self.run_command_and_parse(&format!("STATUS {} {}", mailbox_name, status_data_items))
	}

	/// Runs a command and checks if it returns OK.
	pub fn run_command_and_check_ok(&mut self, command: &str) -> Result<()> {
		let lines = try!(self.run_command_and_read_response(command));
		parse_response_ok(lines)
	}

	// Run a command and parse the status response.
	pub fn run_command_and_parse(&mut self, command: &str) -> Result<Vec<String>> {
		let lines = try!(self.run_command_and_read_response(command));
		parse_response(lines)
	}

	/// Runs any command passed to it.
	pub fn run_command(&mut self, untagged_command: &str) -> Result<()> {
		let command = self.create_command(untagged_command.to_string());
		self.write_line(command.into_bytes().as_slice())
	}

	pub fn run_command_and_read_response(&mut self, untagged_command: &str) -> Result<Vec<String>> {
		try!(self.run_command(untagged_command));
		self.read_response()
	}

	fn read_response(&mut self) -> Result<Vec<String>> {
		let mut found_tag_line = false;
		let start_str = format!("{}{} ", TAG_PREFIX, self.tag);
		let mut lines: Vec<String> = Vec::new();

		while !found_tag_line {
			let raw_data = try!(self.readline());
			let line = String::from_utf8(raw_data).unwrap();
			lines.push(line.clone());
			if (&*line).starts_with(&*start_str) {
				found_tag_line = true;
			}
		}

		Ok(lines)
	}

	fn read_greeting(&mut self) -> Result<()> {
		try!(self.readline());
		Ok(())
	}

	fn readline(&mut self) -> Result<Vec<u8>> {
		let mut line_buffer: Vec<u8> = Vec::new();
		while line_buffer.len() < 2 || (line_buffer[line_buffer.len()-1] != LF && line_buffer[line_buffer.len()-2] != CR) {
			let byte_buffer: &mut [u8] = &mut [0];
			try!(self.stream.read(byte_buffer));
			line_buffer.push(byte_buffer[0]);
		}

		if self.debug {
			let mut line = line_buffer.clone();
			// Remove CRLF
			line.truncate(line_buffer.len()-2);
			print!("S: {}\n", String::from_utf8(line).unwrap());
		}

		Ok(line_buffer)
	}

	fn create_command(&mut self, command: String) -> String {
		self.tag += 1;
		let command = format!("{}{} {}", TAG_PREFIX, self.tag, command);
		return command;
	}

	fn write_line(&mut self, buf: &[u8]) -> Result<()> {
		try!(self.stream.write_all(buf));
		try!(self.stream.write_all(&[CR, LF]));
		if self.debug {
			print!("C: {}\n", String::from_utf8(buf.to_vec()).unwrap());
		}
		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use super::super::mock_stream::MockStream;
	use super::super::mailbox::Mailbox;
	use super::super::error::Result;

	#[test]
	fn read_response() {
		let response = "a0 OK Logged in.\r\n";
		let expected_response: Vec<String> = vec![response.to_string()];
		let mock_stream = MockStream::new(response.as_bytes().to_vec());
		let mut client = Client::new(mock_stream);
		let actual_response = client.read_response().unwrap();
		assert!(expected_response == actual_response, "expected response doesn't equal actual");
	}

	#[test]
	fn read_greeting() {
		let greeting = "* OK Dovecot ready.\r\n";
		let mock_stream = MockStream::new(greeting.as_bytes().to_vec());
		let mut client = Client::new(mock_stream);
		client.read_greeting().unwrap();
	}

	#[test]
	#[should_panic]
	fn readline_err() {
		// TODO Check the error test
		let mock_stream = MockStream::new_err();
		let mut client = Client::new(mock_stream);
		client.readline().unwrap();
	}

	#[test]
	fn create_command() {
		let base_command = "CHECK";
		let mock_stream = MockStream::new(Vec::new());
		let mut imap_stream = Client::new(mock_stream);

		let expected_command = format!("a1 {}", base_command);
		let command = imap_stream.create_command(String::from(base_command));
		assert!(command == expected_command, "expected command doesn't equal actual command");

		let expected_command2 = format!("a2 {}", base_command);
		let command2 = imap_stream.create_command(String::from(base_command));
		assert!(command2 == expected_command2, "expected command doesn't equal actual command");
	}

	#[test]
	fn login() {
		let response = b"a1 OK Logged in\r\n".to_vec();
		let username = "username";
		let password = "password";
		let command = format!("a1 LOGIN {} {}\r\n", username, password);
		let mock_stream = MockStream::new(response);
		let mut client = Client::new(mock_stream);
		client.login(username, password).unwrap();
		assert!(client.stream.written_buf == command.as_bytes().to_vec(), "Invalid login command");
	}

	#[test]
	fn logout() {
		let response = b"a1 OK Logout completed.\r\n".to_vec();
		let command = format!("a1 LOGOUT\r\n");
		let mock_stream = MockStream::new(response);
		let mut client = Client::new(mock_stream);
		client.logout().unwrap();
		assert!(client.stream.written_buf == command.as_bytes().to_vec(), "Invalid logout command");
	}

	#[test]
	fn rename() {
		let response = b"a1 OK RENAME completed\r\n".to_vec();
		let current_mailbox_name = "INBOX";
		let new_mailbox_name = "NEWINBOX";
		let command = format!("a1 RENAME {} {}\r\n", current_mailbox_name, new_mailbox_name);
		let mock_stream = MockStream::new(response);
		let mut client = Client::new(mock_stream);
		client.rename(current_mailbox_name, new_mailbox_name).unwrap();
		assert!(client.stream.written_buf == command.as_bytes().to_vec(), "Invalid rename command");
	}

	#[test]
	fn subscribe() {
		let response = b"a1 OK SUBSCRIBE completed\r\n".to_vec();
		let mailbox = "INBOX";
		let command = format!("a1 SUBSCRIBE {}\r\n", mailbox);
		let mock_stream = MockStream::new(response);
		let mut client = Client::new(mock_stream);
		client.subscribe(mailbox).unwrap();
		assert!(client.stream.written_buf == command.as_bytes().to_vec(), "Invalid subscribe command");
	}

	#[test]
	fn unsubscribe() {
		let response = b"a1 OK UNSUBSCRIBE completed\r\n".to_vec();
		let mailbox = "INBOX";
		let command = format!("a1 UNSUBSCRIBE {}\r\n", mailbox);
		let mock_stream = MockStream::new(response);
		let mut client = Client::new(mock_stream);
		client.unsubscribe(mailbox).unwrap();
		assert!(client.stream.written_buf == command.as_bytes().to_vec(), "Invalid unsubscribe command");
	}

	#[test]
	fn expunge() {
		let response = b"a1 OK EXPUNGE completed\r\n".to_vec();
		let mock_stream = MockStream::new(response);
		let mut client = Client::new(mock_stream);
		client.expunge().unwrap();
		assert!(client.stream.written_buf == b"a1 EXPUNGE\r\n".to_vec(), "Invalid expunge command");
	}

	#[test]
	fn check() {
		let response = b"a1 OK CHECK completed\r\n".to_vec();
		let mock_stream = MockStream::new(response);
		let mut client = Client::new(mock_stream);
		client.check().unwrap();
		assert!(client.stream.written_buf == b"a1 CHECK\r\n".to_vec(), "Invalid check command");
	}

	#[test]
	fn examine() {
		let response = b"* FLAGS (\\Answered \\Flagged \\Deleted \\Seen \\Draft)\r\n\
			* OK [PERMANENTFLAGS ()] Read-only mailbox.\r\n\
			* 1 EXISTS\r\n\
			* 1 RECENT\r\n\
			* OK [UNSEEN 1] First unseen.\r\n\
			* OK [UIDVALIDITY 1257842737] UIDs valid\r\n\
			* OK [UIDNEXT 2] Predicted next UID\r\n\
			a1 OK [READ-ONLY] Select completed.\r\n".to_vec();
		let expected_mailbox = Mailbox {
			flags: String::from("(\\Answered \\Flagged \\Deleted \\Seen \\Draft)"),
			exists: 1,
			recent: 1,
			unseen: Some(1),
			permanent_flags: Some(String::from("()")),
			uid_next: Some(2),
			uid_validity: Some(1257842737)
		};
		let mailbox_name = "INBOX";
		let command = format!("a1 EXAMINE {}\r\n", mailbox_name);
		let mock_stream = MockStream::new(response);
		let mut client = Client::new(mock_stream);
		let mailbox = client.examine(mailbox_name).unwrap();
		assert!(client.stream.written_buf == command.as_bytes().to_vec(), "Invalid examine command");
		assert!(mailbox == expected_mailbox, "Unexpected mailbox returned");
	}

	#[test]
	fn select() {
		let response = b"* FLAGS (\\Answered \\Flagged \\Deleted \\Seen \\Draft)\r\n\
			* OK [PERMANENTFLAGS (\\* \\Answered \\Flagged \\Deleted \\Draft \\Seen)] Read-only mailbox.\r\n\
			* 1 EXISTS\r\n\
			* 1 RECENT\r\n\
			* OK [UNSEEN 1] First unseen.\r\n\
			* OK [UIDVALIDITY 1257842737] UIDs valid\r\n\
			* OK [UIDNEXT 2] Predicted next UID\r\n\
			a1 OK [READ-ONLY] Select completed.\r\n".to_vec();
		let expected_mailbox = Mailbox {
			flags: String::from("(\\Answered \\Flagged \\Deleted \\Seen \\Draft)"),
			exists: 1,
			recent: 1,
			unseen: Some(1),
			permanent_flags: Some(String::from("(\\* \\Answered \\Flagged \\Deleted \\Draft \\Seen)")),
			uid_next: Some(2),
			uid_validity: Some(1257842737)
		};
		let mailbox_name = "INBOX";
		let command = format!("a1 SELECT {}\r\n", mailbox_name);
		let mock_stream = MockStream::new(response);
		let mut client = Client::new(mock_stream);
		let mailbox = client.select(mailbox_name).unwrap();
		assert!(client.stream.written_buf == command.as_bytes().to_vec(), "Invalid select command");
		assert!(mailbox == expected_mailbox, "Unexpected mailbox returned");
	}

	#[test]
	fn capability() {
		let response = b"* CAPABILITY IMAP4rev1 STARTTLS AUTH=GSSAPI LOGINDISABLED\r\n\
			a1 OK CAPABILITY completed\r\n".to_vec();
		let expected_capabilities = vec!["IMAP4rev1", "STARTTLS", "AUTH=GSSAPI", "LOGINDISABLED"];
		let mock_stream = MockStream::new(response);
		let mut client = Client::new(mock_stream);
		let capabilities = client.capability().unwrap();
		assert!(client.stream.written_buf == b"a1 CAPABILITY\r\n".to_vec(), "Invalid capability command");
		assert!(capabilities == expected_capabilities, "Unexpected capabilities response");
	}

	#[test]
	fn create() {
		let response = b"a1 OK CREATE completed\r\n".to_vec();
		let mailbox_name = "INBOX";
		let command = format!("a1 CREATE {}\r\n", mailbox_name);
		let mock_stream = MockStream::new(response);
		let mut client = Client::new(mock_stream);
		client.create(mailbox_name).unwrap();
		assert!(client.stream.written_buf == command.as_bytes().to_vec(), "Invalid create command");
	}

	#[test]
	fn delete() {
		let response = b"a1 OK DELETE completed\r\n".to_vec();
		let mailbox_name = "INBOX";
		let command = format!("a1 DELETE {}\r\n", mailbox_name);
		let mock_stream = MockStream::new(response);
		let mut client = Client::new(mock_stream);
		client.delete(mailbox_name).unwrap();
		assert!(client.stream.written_buf == command.as_bytes().to_vec(), "Invalid delete command");
	}

	#[test]
	fn noop() {
		let response = b"a1 OK NOOP completed\r\n".to_vec();
		let mock_stream = MockStream::new(response);
		let mut client = Client::new(mock_stream);
		client.noop().unwrap();
		assert!(client.stream.written_buf == b"a1 NOOP\r\n".to_vec(), "Invalid noop command");
	}

	#[test]
	fn close() {
		let response = b"a1 OK CLOSE completed\r\n".to_vec();
		let mock_stream = MockStream::new(response);
		let mut client = Client::new(mock_stream);
		client.close().unwrap();
		assert!(client.stream.written_buf == b"a1 CLOSE\r\n".to_vec(), "Invalid close command");
	}

	#[test]
	fn store() {
		generic_store(" ", |mut c, set, query| c.store(set, query));
	}

	#[test]
	fn uid_store() {
		generic_store(" UID ", |mut c, set, query| c.uid_store(set, query));
	}

	fn generic_store<F, T>(prefix: &str, op: F)
		where F: FnOnce(&mut Client<MockStream>, &str, &str) -> Result<T> {

		let res = "* 2 FETCH (FLAGS (\\Deleted \\Seen))\r\n\
			* 3 FETCH (FLAGS (\\Deleted))\r\n\
			* 4 FETCH (FLAGS (\\Deleted \\Flagged \\Seen))\r\n\
			a1 OK STORE completed\r\n";

		generic_with_uid(
			res,
			"STORE",
			"2.4",
			"+FLAGS (\\Deleted)",
			prefix,
			op,
		);
	}

	#[test]
	fn copy() {
		generic_copy(" ", |mut c, set, query| c.copy(set, query))
	}

	#[test]
	fn uid_copy() {
		generic_copy(" UID ", |mut c, set, query| c.uid_copy(set, query))
	}

	fn generic_copy<F, T>(prefix: &str, op: F)
		where F: FnOnce(&mut Client<MockStream>, &str, &str) -> Result<T> {

		generic_with_uid(
			"OK COPY completed\r\n",
			"COPY",
			"2:4",
			"MEETING",
			prefix,
			op,
		);
	}

	#[test]
	fn fetch() {
		generic_fetch(" ", |mut c, seq, query| c.fetch(seq, query))
	}

	#[test]
	fn uid_fetch() {
		generic_fetch(" UID ", |mut c, seq, query| c.uid_fetch(seq, query))
	}

	fn generic_fetch<F, T>(prefix: &str, op: F)
		where F: FnOnce(&mut Client<MockStream>, &str, &str) -> Result<T> {

		generic_with_uid(
			"OK FETCH completed\r\n",
			"FETCH",
			"1",
			"BODY[]",
			prefix,
			op
		);
	}

	fn generic_with_uid<F, T>(
		res: &str,
		cmd: &str,
		seq: &str,
		query: &str,
		prefix: &str,
		op: F) where F: FnOnce(&mut Client<MockStream>, &str, &str) -> Result<T>,
	{

		let resp = format!("a1 {}\r\n", res).as_bytes().to_vec();
		let line = format!("a1{}{} {} {}\r\n", prefix, cmd, seq, query);
		let mut client = Client::new(MockStream::new(resp));
		let _ = op(&mut client, seq, query);
		assert!(client.stream.written_buf == line.as_bytes().to_vec(), "Invalid command");
	}
}
