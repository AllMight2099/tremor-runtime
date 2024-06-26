// Copyright 2020-2021, The Tremor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Encodes raw data into base64 encoded bytes.

use super::Stateless;
use tremor_common::base64;

#[derive(Default)]
pub(crate) struct Base64 {}
impl Stateless for Base64 {
    fn name(&self) -> &str {
        "base64"
    }

    fn process(&self, data: &[u8]) -> anyhow::Result<Vec<Vec<u8>>> {
        Ok(vec![base64::encode(data).as_bytes().to_vec()])
    }
}

#[cfg(test)]
mod test {
    use super::*;
    #[test]
    fn base64() {
        let post = Base64 {};
        let data: [u8; 0] = [];

        assert_eq!(post.process(&data).ok(), Some(vec![vec![]]));

        assert_eq!(post.name(), "base64");

        assert_eq!(post.process(b"\n").ok(), Some(vec![b"Cg==".to_vec()]));

        assert_eq!(post.process(b"snot").ok(), Some(vec![b"c25vdA==".to_vec()]));
    }
}
