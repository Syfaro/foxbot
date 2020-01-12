use serde::{Deserialize, Serialize};

/// Lookup is information returned when attempting to search for a file by
/// url or filename. It contains no additional information about the image
/// besides its ID on FurAffinity.
#[derive(Debug, Deserialize, Serialize)]
pub struct Lookup {
    pub id: usize,
    pub url: String,
    pub filename: String,
}

/// ImageLookup is information returned when attempting to reverse image search.
/// It includes a distance, which is the hamming distance between the provided
/// image and the image in the database.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ImageLookup {
    pub id: usize,
    pub distance: u64,
    pub hash: i64,
    pub url: String,
    pub filename: String,
    pub artist_id: i32,
    pub artist_name: String,
}

/// ImageHashLookup is information returned when attempting to lookup a hash.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ImageHashLookup {
    pub id: usize,
    pub hash: i64,
    pub url: String,
    pub filename: String,
    pub artist_id: i32,
    pub artist_name: String,
}
