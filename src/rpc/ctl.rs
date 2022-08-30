use internet2::Api;
use strict_encoding::{StrictDecode, StrictEncode};

#[derive(Clone, Debug, Display, From, StrictDecode, StrictEncode, Api)]
#[api(encoding = "strict")]
#[non_exhaustive]
pub enum Ctl {
    #[api(type = 100)]
    #[display("Dummy")]
    Dummy,
}
