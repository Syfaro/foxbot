use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "site", content = "site_info")]
pub enum SiteInfo {
    FurAffinity(FurAffinityFile),
    #[serde(rename = "e621")]
    E621(E621File),
    Twitter,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct FurAffinityFile {
    pub file_id: i32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct E621File {
    pub sources: Option<Vec<String>>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct File {
    pub id: i32,
    pub site_id: i64,
    pub url: String,
    pub filename: String,
    pub artists: Option<Vec<String>>,
    pub hash: Option<i64>,
    pub distance: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(flatten)]
    pub site_info: Option<SiteInfo>,
}

impl File {
    pub fn site_name(&self) -> &'static str {
        match &self.site_info {
            Some(SiteInfo::Twitter) => "Twitter",
            Some(SiteInfo::FurAffinity(_)) => "FurAffinity",
            Some(SiteInfo::E621(_)) => "e621",
            _ => unreachable!(),
        }
    }

    pub fn url(&self) -> String {
        match &self.site_info {
            Some(SiteInfo::Twitter) => format!(
                "https://twitter.com/{}/status/{}",
                self.artists.as_ref().unwrap().iter().next().unwrap(),
                self.site_id
            ),
            Some(SiteInfo::FurAffinity(_)) => {
                format!("https://www.furaffinity.net/view/{}/", self.site_id)
            }
            Some(SiteInfo::E621(_)) => format!("https://e621.net/post/show/{}", self.site_id),
            _ => unreachable!(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Matches {
    pub hash: i64,
    pub matches: Vec<File>,
}
