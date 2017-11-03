#[derive(Debug, Eq, PartialEq)]
pub struct Fetch {
    pub message: u32,
    pub flags: Vec<String>,
    pub uid: Option<u32>,
}
