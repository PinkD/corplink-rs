use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, PartialOrd)]
pub enum State {
    Init,
    Login,
}

impl fmt::Display for State {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            State::Init => write!(f, "Init"),
            State::Login => write!(f, "Login"),
        }
    }
}
