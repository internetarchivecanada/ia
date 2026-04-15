use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Full response from GET /metadata/{identifier}
#[derive(Debug, Clone, Deserialize, Serialize)]
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

    /// All other top-level fields not captured above (e.g., extracted_metadata,
    /// reviews, speech_vs_music_asr, etc.).
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// Item-level metadata fields.
///
/// All fields use [`MetadataValue`] so deserialization never fails regardless
/// of what the API returns. Use `.first()` for convenient string access,
/// `.is_other()` to detect unexpected types, and `.as_value()` for raw access.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct MetadataFields {
    pub identifier: Option<MetadataValue>,
    pub title: Option<MetadataValue>,
    pub description: Option<MetadataValue>,
    pub mediatype: Option<MetadataValue>,
    pub collection: Option<MetadataValue>,
    pub creator: Option<MetadataValue>,
    pub date: Option<MetadataValue>,
    pub subject: Option<MetadataValue>,
    pub language: Option<MetadataValue>,
    pub publicdate: Option<MetadataValue>,
    pub addeddate: Option<MetadataValue>,
    pub uploader: Option<MetadataValue>,

    /// All other metadata fields not captured above.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// A metadata field value that accepts any JSON type without failing.
///
/// IA metadata is schemaless — any field can be a string, array of strings,
/// number, object, or anything else. `MetadataValue` tries variants in order:
/// `Single(String)` first, then `Multiple(Vec<String>)`, then `Other(Value)`
/// as a catch-all. Deserialization **cannot fail**.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(untagged)]
pub enum MetadataValue {
    /// A single string value (the most common case).
    Single(String),
    /// An array of strings (e.g., multiple collections, subjects).
    Multiple(Vec<String>),
    /// Any other JSON value (number, object, mixed array, etc.).
    /// Present but not in an expected string form.
    Other(serde_json::Value),
}

/// Backward-compatibility alias.
pub type StringOrVec = MetadataValue;

impl MetadataValue {
    /// Get the first string value, or `""` for non-string types.
    pub fn first(&self) -> &str {
        match self {
            MetadataValue::Single(s) => s,
            MetadataValue::Multiple(v) => v.first().map(|s| s.as_str()).unwrap_or(""),
            MetadataValue::Other(_) => "",
        }
    }

    /// Get all values as strings, or `[]` for non-string types.
    pub fn to_vec(&self) -> Vec<&str> {
        match self {
            MetadataValue::Single(s) => vec![s.as_str()],
            MetadataValue::Multiple(v) => v.iter().map(|s| s.as_str()).collect(),
            MetadataValue::Other(_) => vec![],
        }
    }

    /// True if the value didn't match `String` or `Vec<String>`.
    pub fn is_other(&self) -> bool {
        matches!(self, MetadataValue::Other(_))
    }

    /// Get the underlying `serde_json::Value` for any variant.
    pub fn as_value(&self) -> serde_json::Value {
        match self {
            MetadataValue::Single(s) => serde_json::Value::String(s.clone()),
            MetadataValue::Multiple(v) => serde_json::Value::Array(
                v.iter()
                    .map(|s| serde_json::Value::String(s.clone()))
                    .collect(),
            ),
            MetadataValue::Other(v) => v.clone(),
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
fn deserialize_optional_string_u64<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<u64>, D::Error>
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
        Some(StringOrNum::Str(s)) => s.parse::<u64>().map(Some).map_err(de::Error::custom),
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
        assert_eq!(
            item.metadata.identifier.as_ref().map(|v| v.first()),
            Some("nasa")
        );
        assert_eq!(item.metadata.title.as_ref().unwrap().first(), "NASA Images");
        assert_eq!(item.files.len(), 2);
        assert_eq!(item.files[0].name, "photo.jpg");
        assert_eq!(item.files[0].size, Some(4200000));
        assert_eq!(item.files[0].mtime, Some(1700000000));
        assert_eq!(item.files[1].source.as_deref(), Some("derivative"));
        assert_eq!(item.files[1].original.as_deref(), Some("photo.jpg"));
    }

    #[test]
    fn metadata_value_single() {
        let json = r#""hello""#;
        let v: MetadataValue = serde_json::from_str(json).unwrap();
        assert_eq!(v.first(), "hello");
        assert_eq!(v.to_vec(), vec!["hello"]);
        assert!(!v.is_other());
    }

    #[test]
    fn metadata_value_multiple() {
        let json = r#"["a", "b", "c"]"#;
        let v: MetadataValue = serde_json::from_str(json).unwrap();
        assert_eq!(v.first(), "a");
        assert_eq!(v.to_vec(), vec!["a", "b", "c"]);
        assert!(!v.is_other());
    }

    #[test]
    fn metadata_value_number_falls_through_to_other() {
        let json = "42";
        let v: MetadataValue = serde_json::from_str(json).unwrap();
        assert!(v.is_other());
        assert_eq!(v.first(), "");
        assert!(v.to_vec().is_empty());
        assert_eq!(v.as_value(), serde_json::json!(42));
    }

    #[test]
    fn metadata_value_object_falls_through_to_other() {
        let json = r#"{"nested": "object"}"#;
        let v: MetadataValue = serde_json::from_str(json).unwrap();
        assert!(v.is_other());
        assert_eq!(v.as_value(), serde_json::json!({"nested": "object"}));
    }

    #[test]
    fn metadata_value_mixed_array_falls_through_to_other() {
        let json = r#"[1, "two", 3]"#;
        let v: MetadataValue = serde_json::from_str(json).unwrap();
        assert!(v.is_other());
        assert_eq!(v.as_value(), serde_json::json!([1, "two", 3]));
    }

    #[test]
    fn metadata_value_as_value_roundtrip() {
        let single: MetadataValue = serde_json::from_str(r#""hello""#).unwrap();
        assert_eq!(single.as_value(), serde_json::json!("hello"));

        let multi: MetadataValue = serde_json::from_str(r#"["a", "b"]"#).unwrap();
        assert_eq!(multi.as_value(), serde_json::json!(["a", "b"]));
    }

    #[test]
    fn metadata_value_bool_falls_through_to_other() {
        let v: MetadataValue = serde_json::from_str("true").unwrap();
        assert!(v.is_other());
        assert_eq!(v.as_value(), serde_json::json!(true));
    }

    #[test]
    fn metadata_value_null_falls_through_to_other() {
        let v: MetadataValue = serde_json::from_str("null").unwrap();
        assert!(v.is_other());
        assert_eq!(v.as_value(), serde_json::json!(null));
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
    fn date_as_string() {
        let json = r#"{"date": "2004"}"#;
        let m: MetadataFields = serde_json::from_str(json).unwrap();
        assert_eq!(m.date.as_ref().unwrap().first(), "2004");
        assert!(!m.date.as_ref().unwrap().is_other());
    }

    #[test]
    fn date_as_array_no_longer_crashes() {
        let json = r#"{"date": ["2004", "December 6, 2004", "December 6, 2004"]}"#;
        let m: MetadataFields = serde_json::from_str(json).unwrap();
        let date = m.date.unwrap();
        assert_eq!(date.first(), "2004");
        assert_eq!(
            date.to_vec(),
            vec!["2004", "December 6, 2004", "December 6, 2004"]
        );
        assert!(!date.is_other());
    }

    #[test]
    fn unexpected_field_type_preserved_in_other() {
        let json = r#"{"identifier": "test", "mediatype": 42}"#;
        let m: MetadataFields = serde_json::from_str(json).unwrap();
        assert_eq!(m.identifier.as_ref().unwrap().first(), "test");
        // mediatype is a number — lands in Other, not a crash
        let mt = m.mediatype.unwrap();
        assert!(mt.is_other());
        assert_eq!(mt.as_value(), serde_json::json!(42));
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

    #[test]
    fn unknown_top_level_fields_preserved() {
        let json = r#"{
            "metadata": {"identifier": "test-item"},
            "files": [],
            "server": "ia802304.us.archive.org",
            "extracted_metadata": {
                "_ai_request_info": {
                    "model": "gpt-5-nano-2025-08-07",
                    "total_tokens": 21509
                },
                "metadata": {
                    "title": "Essays & Addresses",
                    "creator": ["Friedrich Von Hügel"]
                }
            },
            "speech_vs_music_asr": {"some_key": "some_value"},
            "reviews": [{"stars": 5, "reviewer": "someone"}]
        }"#;

        let item: ItemMetadata = serde_json::from_str(json).unwrap();

        // extracted_metadata must survive deserialization
        let extracted = item
            .extra
            .get("extracted_metadata")
            .expect("extracted_metadata must be preserved in ItemMetadata.extra");
        assert_eq!(
            extracted["metadata"]["title"],
            serde_json::json!("Essays & Addresses")
        );
        assert_eq!(
            extracted["_ai_request_info"]["model"],
            serde_json::json!("gpt-5-nano-2025-08-07")
        );

        // other unknown fields too
        assert!(item.extra.contains_key("speech_vs_music_asr"));
        assert!(item.extra.contains_key("reviews"));

        // round-trip: serialize back to JSON and verify fields survive
        let reserialized: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&item).unwrap()).unwrap();
        assert!(reserialized.get("extracted_metadata").is_some());
        assert!(reserialized.get("speech_vs_music_asr").is_some());
        assert!(reserialized.get("reviews").is_some());
    }
}
