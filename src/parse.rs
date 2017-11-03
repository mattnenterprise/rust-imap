use regex::Regex;
use nom::IResult;
use imap_proto::{self, Response};

use super::types::*;
use super::error::{Error, ParseError, Result};

pub fn parse_authenticate_response(line: String) -> Result<String> {
    let authenticate_regex = Regex::new("^+(.*)\r\n").unwrap();

    for cap in authenticate_regex.captures_iter(line.as_str()) {
        let data = cap.get(1).map(|x| x.as_str()).unwrap_or("");
        return Ok(String::from(data));
    }

    Err(Error::Parse(ParseError::Authentication(line)))
}

enum MapOrNot<'a, T: 'a> {
    Map(T),
    Not(Response<'a>),
}

fn parse_many<T, F>(mut lines: &[u8], mut map: F) -> Result<Vec<T>>
where
    F: FnMut(Response) -> MapOrNot<T>,
{
    let mut things = Vec::new();
    loop {
        match imap_proto::parse_response(lines) {
            IResult::Done(rest, resp) => {
                lines = rest;

                match map(resp) {
                    MapOrNot::Map(t) => things.push(t),
                    MapOrNot::Not(resp) => break Err(resp.into()),
                }

                if lines.is_empty() {
                    break Ok(things);
                }
            }
            _ => {
                break Err(Error::Parse(ParseError::Invalid(lines.to_vec())));
            }
        }
    }
}

pub fn parse_names(lines: &[u8]) -> Result<Vec<Name>> {
    use imap_proto::MailboxDatum;
    parse_many(lines, |resp| match resp {
        // https://github.com/djc/imap-proto/issues/4
        Response::MailboxData(MailboxDatum::List(attrs, delim, name)) => MapOrNot::Map(Name {
            attributes: attrs.into_iter().map(|s| s.to_string()).collect(),
            delimiter: delim.to_string(),
            name: name.to_string(),
        }),
        resp => MapOrNot::Not(resp),
    })
}

pub fn parse_fetches(lines: &[u8]) -> Result<Vec<Fetch>> {
    parse_many(lines, |resp| match resp {
        Response::Fetch(num, attrs) => {
            let mut fetch = Fetch {
                message: num,
                flags: vec![],
                uid: None,
            };

            for attr in attrs {
                use imap_proto::AttributeValue;
                match attr {
                    AttributeValue::Flags(flags) => {
                        fetch.flags.extend(flags.into_iter().map(|s| s.to_string()))
                    }
                    AttributeValue::Uid(uid) => fetch.uid = Some(uid),
                    _ => {}
                }
            }

            MapOrNot::Map(fetch)
        }
        resp => MapOrNot::Not(resp),
    })
}

pub fn parse_capability<'a>(mut lines: &'a [u8]) -> Result<Vec<&'a str>> {
    let mut capabilities = Vec::new();
    loop {
        match imap_proto::parse_response(lines) {
            IResult::Done(rest, Response::Capabilities(c)) => {
                lines = rest;
                capabilities.extend(c);

                if lines.is_empty() {
                    break Ok(capabilities);
                }
            }
            IResult::Done(_, resp) => {
                break Err(resp.into());
            }
            _ => {
                break Err(Error::Parse(ParseError::Invalid(lines.to_vec())));
            }
        }
    }
}

pub fn parse_mailbox(mut lines: &[u8]) -> Result<Mailbox> {
    let mut mailbox = Mailbox::default();

    loop {
        match imap_proto::parse_response(lines) {
            IResult::Done(rest, Response::Data(status, rcode, _)) => {
                lines = rest;

                if let imap_proto::Status::Ok = status {
                } else {
                    // how can this happen for a Response::Data?
                    unreachable!();
                }

                use imap_proto::ResponseCode;
                match rcode {
                    Some(ResponseCode::UidValidity(uid)) => {
                        mailbox.uid_validity = Some(uid);
                    }
                    Some(ResponseCode::UidNext(unext)) => {
                        mailbox.uid_next = Some(unext);
                    }
                    Some(ResponseCode::PermanentFlags(flags)) => {
                        mailbox
                            .permanent_flags
                            .extend(flags.into_iter().map(|s| s.to_string()));
                    }
                    // TODO: UNSEEN
                    // https://github.com/djc/imap-proto/issues/2
                    _ => {}
                }
            }
            IResult::Done(rest, Response::MailboxData(m)) => {
                lines = rest;

                use imap_proto::MailboxDatum;
                match m {
                    MailboxDatum::Exists(e) => {
                        mailbox.exists = e;
                    }
                    MailboxDatum::Recent(r) => {
                        mailbox.recent = r;
                    }
                    MailboxDatum::Flags(flags) => {
                        mailbox
                            .flags
                            .extend(flags.into_iter().map(|s| s.to_string()));
                    }
                    MailboxDatum::List(..) => {}
                }
            }
            IResult::Done(_, resp) => {
                break Err(resp.into());
            }
            _ => {
                break Err(Error::Parse(ParseError::Invalid(lines.to_vec())));
            }
        }

        if lines.is_empty() {
            break Ok(mailbox);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_capability_test() {
        let expected_capabilities = vec![
            String::from("IMAP4rev1"),
            String::from("STARTTLS"),
            String::from("AUTH=GSSAPI"),
            String::from("LOGINDISABLED"),
        ];
        let lines = b"* CAPABILITY IMAP4rev1 STARTTLS AUTH=GSSAPI LOGINDISABLED\r\n";
        let capabilities = parse_capability(lines).unwrap();
        assert!(
            capabilities == expected_capabilities,
            "Unexpected capabilities parse response"
        );
    }

    #[test]
    #[should_panic]
    fn parse_capability_invalid_test() {
        let lines = b"* JUNK IMAP4rev1 STARTTLS AUTH=GSSAPI LOGINDISABLED\r\n";
        parse_capability(lines).unwrap();
    }

    #[test]
    fn parse_names_test() {
        let lines = b"* LIST (\\HasNoChildren) \".\" \"INBOX\"\r\n";
        let names = parse_names(lines).unwrap();
        assert_eq!(
            vec![
                Name {
                    attributes: vec!["\\HasNoChildren".to_string()],
                    delimiter: ".".to_string(),
                    name: "INBOX".to_string(),
                },
            ],
            names
        );
    }

    #[test]
    fn parse_fetches_test() {
        let lines = b"\
                    * 24 FETCH (FLAGS (\\Seen) UID 4827943)\r\n\
                    * 25 FETCH (FLAGS (\\Seen))\r\n";
        let fetches = parse_fetches(lines).unwrap();
        assert_eq!(
            vec![
                Fetch {
                    message: 24,
                    flags: vec!["\\Seen".to_string()],
                    uid: Some(4827943),
                },
                Fetch {
                    message: 25,
                    flags: vec!["\\Seen".to_string()],
                    uid: None,
                },
            ],
            fetches
        );
    }
}
