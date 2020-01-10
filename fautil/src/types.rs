use serde::Deserialize;

/// Lookup is information returned when attempting to search for a file by
/// url or filename. It contains no additional information about the image
/// besides its ID on FurAffinity.
#[derive(Debug, Deserialize)]
pub struct Lookup {
    pub id: usize,
    pub url: String,
    pub filename: String,
}

/// ImageLookup is information returned when attempting to reverse image search.
/// It includes a distance, which is the hamming distance between the provided
/// image and the image in the database.
#[derive(Debug, Deserialize)]
pub struct ImageLookup {
    pub id: usize,
    pub distance: usize,
    pub url: String,
    pub filename: String,
}
