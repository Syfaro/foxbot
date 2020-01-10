use serde::Serialize;

use crate::requests::*;

/// FileType is a possible file for a Telegram request.
#[derive(Serialize, Clone)]
#[serde(untagged)]
pub enum FileType {
    /// URL contains a String with the URL to pass to Telegram.
    URL(String),
    /// FileID is an ID about a file already on Telegram's servers.
    FileID(String),
    /// Attach is a specific type used to attach multiple files to a request.
    Attach(String),
    /// Bytes requires a filename in addition to the file bytes, which it
    /// then uploads to Telegram.
    Bytes(String, Vec<u8>),
    /// Missing means that a file should have been specified but has not.
    ///
    /// This is used for the Default implementation, causes a panic on upload.
    Missing,
}

impl Default for FileType {
    /// Create a FileType with a Missing file. Note that attempting to upload
    /// this file will result in a panic.
    fn default() -> Self {
        FileType::Missing
    }
}

impl std::fmt::Debug for FileType {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            FileType::URL(url) => write!(f, "FileType URL: {}", url),
            FileType::FileID(file_id) => write!(f, "FileType FileID: {}", file_id),
            FileType::Attach(attach) => write!(f, "FileType Attach: {}", attach),
            FileType::Bytes(name, bytes) => {
                write!(f, "FileType Bytes: {} with len {}", name, bytes.len())
            }
            FileType::Missing => write!(f, "FileType Missing!!"),
        }
    }
}

impl FileType {
    /// Returns if this file is a type that gets uploaded to Telegram.
    /// Most types are simply passed through as strings.
    pub fn needs_upload(&self) -> bool {
        match self {
            FileType::Bytes(_, _) => true,
            _ => false,
        }
    }

    /// Get a multipart Part for the file.
    pub fn file(&self) -> Option<reqwest::multipart::Part> {
        match self {
            FileType::Bytes(file_name, bytes) => {
                Some(reqwest::multipart::Part::bytes(bytes.clone()).file_name(file_name.clone()))
            }
            _ => None,
        }
    }
}

/// Attempt to remove body for types that are getting uploaded.
/// It also converts files into attachments with names based on filenames.
///
/// This will panic if you attempt to upload a Missing file.
pub fn clean_input_media<S>(input_media: &[InputMedia], s: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    use serde::ser::SerializeSeq;

    let mut seq = s.serialize_seq(Some(input_media.len()))?;

    for elem in input_media {
        let file = elem.get_file();

        if let FileType::Missing = file {
            panic!("tried to uploading missing file");
        }

        if file.needs_upload() {
            let new_file = match file {
                FileType::Bytes(file_name, _) => {
                    FileType::Attach(format!("attach://{}", file_name))
                }
                _ => unimplemented!(),
            };

            let new_elem = elem.update_media(new_file);

            seq.serialize_element(&new_elem)?;
        } else {
            seq.serialize_element(elem)?;
        }
    }

    seq.end()
}
