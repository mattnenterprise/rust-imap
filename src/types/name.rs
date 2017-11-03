#[derive(Debug, Eq, PartialEq)]
pub struct Name {
    pub attributes: Vec<String>,
    pub delimiter: String,
    pub name: String,
}
