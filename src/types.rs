use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub enum CoconutEvent {
    Progress {
        display_name: String,
        progress: String,
    },
    Completed {
        display_name: String,
        video_url: String,
        thumb_url: String,
    },
}
