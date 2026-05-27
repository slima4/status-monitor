//! Bumping a constant here surfaces the re-accept panel on next sign-in.
//! Signup binds the current value at INSERT — no SQL default, which would
//! freeze on the v1-era.

pub const TERMS_VERSION: &str = "v1";
pub const PRIVACY_VERSION: &str = "v1";
