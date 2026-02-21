use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Full response from GET /metadata/{identifier}
#[derive(Debug, Clone, Deserialize)]
pub struct ItemMetadata {
    /// The item's metadata fields.
    #[serde(default)]
    pub metadata: MetadataFields,

    /// Files contained in the item.
    #[serde(default)]
    pub files: Vec<FileMetadata>,

    /// Server hosting the item (e.g., "ia802304.us.archive.org").
    pub server: Option<String>,

    /// Primary directory server.
    pub d1: Option<String>,

    /// Secondary directory server.
    pub d2: Option<String>,

    /// Directory path on server.
    pub dir: Option<String>,

    /// Number of files that have been cached.
    pub files_count: Option<u64>,

    /// Total size of the item in bytes.
    pub item_size: Option<u64>,

    /// Whether the item is "dark" (hidden).
    #[serde(default)]
    pub is_dark: bool,
}

/// Item-level metadata fields.
/// Uses a mix of typed common fields and a catch-all HashMap.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct MetadataFields {
    pub identifier: Option<String>,
    pub title: Option<StringOrVec>,
    pub description: Option<StringOrVec>,
    pub mediatype: Option<String>,
    pub collection: Option<StringOrVec>,
    pub creator: Option<StringOrVec>,
    pub date: Option<String>,
    pub subject: Option<StringOrVec>,
    pub language: Option<StringOrVec>,
    pub publicdate: Option<String>,
    pub addeddate: Option<String>,
    pub uploader: Option<String>,

    /// All other metadata fields not captured above.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// IA metadata fields can be a single string or a vec of strings.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum StringOrVec {
    Single(String),
    Multiple(Vec<String>),
}

impl StringOrVec {
    /// Get the first value (or only value).
    pub fn first(&self) -> &str {
        match self {
            StringOrVec::Single(s) => s,
            StringOrVec::Multiple(v) => v.first().map(|s| s.as_str()).unwrap_or(""),
        }
    }

    /// Get all values as a vec.
    pub fn to_vec(&self) -> Vec<&str> {
        match self {
            StringOrVec::Single(s) => vec![s.as_str()],
            StringOrVec::Multiple(v) => v.iter().map(|s| s.as_str()).collect(),
        }
    }
}

/// Metadata for a single file within an item.
/// Note: IA returns size and mtime as strings, not numbers.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct FileMetadata {
    /// File name (relative path within the item).
    pub name: String,

    /// File source: "original", "derivative", or "metadata".
    pub source: Option<String>,

    /// File format (e.g., "MPEG4", "JPEG", "Text").
    pub format: Option<String>,

    /// MD5 checksum.
    pub md5: Option<String>,

    /// File size in bytes (IA returns as string).
    #[serde(default, deserialize_with = "deserialize_optional_string_u64")]
    pub size: Option<u64>,

    /// Modification time as Unix timestamp (IA returns as string).
    #[serde(default, deserialize_with = "deserialize_optional_string_u64")]
    pub mtime: Option<u64>,

    /// SHA1 checksum.
    pub sha1: Option<String>,

    /// CRC32 checksum.
    pub crc32: Option<String>,

    /// For derivatives: the original file this was derived from.
    pub original: Option<String>,

    /// Rotation (for images/video).
    pub rotation: Option<String>,

    /// All other file metadata fields.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// Custom deserializer for fields that IA returns as strings but are actually u64.
fn deserialize_optional_string_u64<'de, D>(deserializer: D) -> std::result::Result<Option<u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de;

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrNum {
        Str(String),
        Num(u64),
    }

    match Option::<StringOrNum>::deserialize(deserializer)? {
        Some(StringOrNum::Str(s)) => s
            .parse::<u64>()
            .map(Some)
            .map_err(de::Error::custom),
        Some(StringOrNum::Num(n)) => Ok(Some(n)),
        None => Ok(None),
    }
}

/// File source filter for downloads.
#[derive(Debug, Clone, PartialEq)]
pub enum FileSource {
    Original,
    Derivative,
    Metadata,
}

impl FileSource {
    pub fn as_str(&self) -> &str {
        match self {
            FileSource::Original => "original",
            FileSource::Derivative => "derivative",
            FileSource::Metadata => "metadata",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_item_metadata() {
        let json = r#"{
            "metadata": {
                "identifier": "nasa",
                "title": "NASA Images",
                "mediatype": "image",
                "collection": ["nasa_collection", "other_collection"],
                "subject": "space"
            },
            "files": [
                {
                    "name": "photo.jpg",
                    "source": "original",
                    "format": "JPEG",
                    "md5": "abc123",
                    "size": "4200000",
                    "mtime": "1700000000"
                },
                {
                    "name": "photo_thumb.jpg",
                    "source": "derivative",
                    "original": "photo.jpg",
                    "size": "50000"
                }
            ],
            "server": "ia802304.us.archive.org",
            "d1": "ia802304.us.archive.org",
            "d2": "ia902304.us.archive.org",
            "dir": "/7/items/nasa"
        }"#;

        let item: ItemMetadata = serde_json::from_str(json).unwrap();
        assert_eq!(item.metadata.identifier.as_deref(), Some("nasa"));
        assert_eq!(item.metadata.title.as_ref().unwrap().first(), "NASA Images");
        assert_eq!(item.files.len(), 2);
        assert_eq!(item.files[0].name, "photo.jpg");
        assert_eq!(item.files[0].size, Some(4200000));
        assert_eq!(item.files[0].mtime, Some(1700000000));
        assert_eq!(item.files[1].source.as_deref(), Some("derivative"));
        assert_eq!(item.files[1].original.as_deref(), Some("photo.jpg"));
    }

    #[test]
    fn string_or_vec_single() {
        let json = r#""hello""#;
        let v: StringOrVec = serde_json::from_str(json).unwrap();
        assert_eq!(v.first(), "hello");
        assert_eq!(v.to_vec(), vec!["hello"]);
    }

    #[test]
    fn string_or_vec_multiple() {
        let json = r#"["a", "b", "c"]"#;
        let v: StringOrVec = serde_json::from_str(json).unwrap();
        assert_eq!(v.first(), "a");
        assert_eq!(v.to_vec(), vec!["a", "b", "c"]);
    }

    #[test]
    fn file_size_as_string() {
        let json = r#"{"name": "test.txt", "size": "12345"}"#;
        let f: FileMetadata = serde_json::from_str(json).unwrap();
        assert_eq!(f.size, Some(12345));
    }

    #[test]
    fn file_size_as_number() {
        let json = r#"{"name": "test.txt", "size": 12345}"#;
        let f: FileMetadata = serde_json::from_str(json).unwrap();
        assert_eq!(f.size, Some(12345));
    }

    #[test]
    fn file_size_missing() {
        let json = r#"{"name": "test.txt"}"#;
        let f: FileMetadata = serde_json::from_str(json).unwrap();
        assert_eq!(f.size, None);
    }

    #[test]
    fn collection_as_single_string() {
        let json = r#"{"collection": "nasa"}"#;
        let m: MetadataFields = serde_json::from_str(json).unwrap();
        assert_eq!(m.collection.unwrap().first(), "nasa");
    }

    #[test]
    fn collection_as_array() {
        let json = r#"{"collection": ["nasa", "images"]}"#;
        let m: MetadataFields = serde_json::from_str(json).unwrap();
        assert_eq!(m.collection.unwrap().to_vec(), vec!["nasa", "images"]);
    }
}
