/// A CLI flag registered by an extension.
#[derive(Clone, Debug)]
pub struct FlagSpec {
    pub name: String,
    pub description: String,
    pub flag_type: FlagType,
    pub default: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlagType {
    Boolean,
    String,
}

impl FlagSpec {
    /// Get the default value as a string.
    pub fn default_value(&self) -> Option<&str> {
        self.default.as_deref()
    }
}
