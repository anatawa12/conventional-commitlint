use hex::FromHexError;
use std::fmt::{Debug, Display, Formatter};
use std::io;
use std::path::PathBuf;
use std::process::Stdio;
use std::str::FromStr;
use tokio::process::Command;

#[derive(Copy, Clone)]
pub struct ObjectHash([u8; ObjectHash::HASH_BYTES]);

impl ObjectHash {
    pub const HASH_BITS: usize = 160;
    pub const HASH_BYTES: usize = Self::HASH_BITS / 8;
    pub const HASH_HEX_LEN: usize = Self::HASH_BITS / 4;

    pub fn from_hex(s: impl AsRef<[u8]>) -> Result<ObjectHash, FromHexError> {
        let s = s.as_ref();
        if s.len() != Self::HASH_HEX_LEN {
            return Err(FromHexError::InvalidStringLength);
        }
        let mut buf = [0u8; Self::HASH_BYTES];
        hex::decode_to_slice(s, &mut buf[..])?;
        Ok(Self(buf))
    }
}

impl Display for ObjectHash {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let mut buf = [0u8; Self::HASH_HEX_LEN];
        hex::encode_to_slice(&self.0, &mut buf).expect("encoding hex");
        // SAFETY: hex is utf8
        f.write_str(unsafe { std::str::from_utf8_unchecked(&buf[..]) })
    }
}

impl Debug for ObjectHash {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        Display::fmt(self, f)
    }
}

impl FromStr for ObjectHash {
    type Err = FromHexError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::from_hex(s)
    }
}

pub(crate) struct GitRepository {
    cwd: PathBuf,
}

impl GitRepository {
    pub(crate) fn new_cwd() -> Self {
        Self {
            cwd: PathBuf::from("."),
        }
    }
}

impl GitRepository {
    fn command(&self, subcommand: &'static str) -> Command {
        let mut command = Command::new("git");
        command.arg(subcommand);
        command.current_dir(&self.cwd);
        command
    }

    pub async fn rev_parse(&self, name: &str) -> io::Result<Option<ObjectHash>> {
        let output = self
            .command("rev-parse")
            .arg(name)
            .stdout(Stdio::piped())
            .spawn()?
            .wait_with_output()
            .await?;
        if !output.status.success() || output.stdout.len() < ObjectHash::HASH_HEX_LEN {
            return Ok(None);
        }
        Ok(ObjectHash::from_hex(&output.stdout[..ObjectHash::HASH_HEX_LEN]).ok())
    }

    pub async fn get_commits(
        &self,
        head: ObjectHash,
        base: ObjectHash,
    ) -> io::Result<Vec<ObjectHash>> {
        let output = self
            .command("log")
            .arg("--format=%H")
            .arg(format!("{head}"))
            .arg(format!("^{base}"))
            .stdout(Stdio::piped())
            .spawn()?
            .wait_with_output()
            .await?;
        if !output.status.success() || output.stdout.len() < ObjectHash::HASH_HEX_LEN {
            return Ok(Vec::new());
        }
        let stdout = output.stdout.as_slice();
        Ok(stdout
            .split(|x: &u8| *x == b'\n')
            .map(|x| x.strip_suffix(b"\r").unwrap_or(x))
            .filter(|x| !x.is_empty())
            .filter_map(|x| ObjectHash::from_hex(x).ok())
            .collect())
    }

    pub async fn get_commit(&self, hash: ObjectHash) -> io::Result<Option<CommitObject>> {
        let output = self
            .command("cat-file")
            .arg("commit")
            .arg(format!("{hash}"))
            .stdout(Stdio::piped())
            .spawn()?
            .wait_with_output()
            .await?;
        if !output.status.success() {
            return Ok(None);
        }

        Ok(CommitObject::parse(output.stdout.as_slice()))
    }
}

#[derive(Debug)]
pub struct CommitObject {
    pub header: Vec<(Vec<u8>, Vec<u8>)>,
    pub message: Vec<u8>,
}

impl CommitObject {
    pub fn parse(mut source: &[u8]) -> Option<CommitObject> {
        fn try_split(source: &[u8], find: u8) -> Option<(&[u8], &[u8])> {
            let p = source.iter().position(|&b| b == find)?;
            let (before, after) = source.split_at(p);
            return Some((before, &after[1..]));
        }

        #[allow(unused_assignments)] // to avoid intellij bug
        let mut line = b"".as_slice();

        let mut header = Vec::<(Vec<u8>, Vec<u8>)>::new();
        loop {
            (line, source) = try_split(source, b'\n')?;
            if line == b"" {
                break;
            }

            let (name, value) = try_split(line, b' ')?;
            let mut value = value.to_vec();

            while source.starts_with(b" ") {
                (line, source) = try_split(source, b'\n')?;
                assert!(line.starts_with(b" "));
                value.push(b'\n');
                value.extend_from_slice(&line[1..]);
            }

            header.push((name.to_vec(), value));
        }

        Some(Self {
            header,
            message: source.to_vec(),
        })
    }
}
