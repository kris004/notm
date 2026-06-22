use crate::{Error, Result};

pub fn validate_tag(tag: &str) -> Result<()> {
    if tag.is_empty() || tag.len() > 200 || tag.contains(char::is_whitespace) || tag.contains('\0')
    {
        Err(Error::InvalidTag(tag.to_string()))
    } else {
        Ok(())
    }
}
