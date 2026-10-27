//! kyme 的简易拼音引擎，Phase 0 的 demo 用。
//!
//! 做三件事：把拼音串切成音节（含简拼和末尾没打完的音节）、在词库里查词、
//! 按一元词频组出整句。之后接入 librime 和神经组句时，音节切分和 lattice 这部分会继续复用。

mod dict;
mod engine;
mod syllable;

pub use dict::{Dictionary, LoadError};
pub use engine::{Candidate, Conversion, Engine};
pub use syllable::{SylId, SyllableTable};
