use internet2::Api;
//use strict_encoding::{StrictDecode, StrictEncode};

#[derive(Clone, Debug, Display, From, Api)]
#[api(encoding = "strict")]
#[non_exhaustive]
pub enum Sync {
    #[api(type = 100)]
    #[display("Dummy")]
    Dummy,
}
