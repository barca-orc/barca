//! `[server] auth`, `BARCA_SERVER_AUTH` and `barca serve --auth`: whether the server requires
//! project API tokens (#187).

use serde::Deserialize;

/// Whether the server requires project API tokens.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AuthMode {
    #[default]
    Off,
    Token,
}
impl std::str::FromStr for AuthMode {
    type Err = String;
    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw {
            "off" => Ok(Self::Off),
            "token" => Ok(Self::Token),
            _ => Err("server auth must be off or token".into()),
        }
    }
}
