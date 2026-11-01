//! Provider persistence: readable ids today, legacy numeric indexes when loading.
use course2md::config::AsrProvider;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Deserialize)]
#[serde(untagged)]
enum Repr {
    Name(String),
    Legacy(u8),
}

fn legacy(index: u8) -> Option<AsrProvider> {
    match index {
        1 => Some(AsrProvider::Coreml),
        2 => Some(AsrProvider::Gpu),
        3 => Some(AsrProvider::Cpu),
        4 => Some(AsrProvider::Npu),
        5 => Some(AsrProvider::Api),
        _ => None,
    }
}

pub fn serialize<S>(value: &Option<AsrProvider>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match value {
        Some(provider) => provider.as_str().serialize(serializer),
        None => serializer.serialize_none(),
    }
}

pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<AsrProvider>, D::Error>
where
    D: Deserializer<'de>,
{
    match Option::<Repr>::deserialize(deserializer)? {
        None => Ok(None),
        Some(Repr::Name(id)) => AsrProvider::from_id(&id)
            .map(Some)
            .ok_or_else(|| serde::de::Error::custom(format!("unknown speech backend: {id}"))),
        Some(Repr::Legacy(index)) => Ok(legacy(index)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(serde::Serialize, serde::Deserialize)]
    struct Holder {
        #[serde(with = "crate::provider_serde")]
        provider: Option<AsrProvider>,
    }

    #[test]
    fn reads_names_nulls_and_legacy_indexes() {
        let name: Holder = serde_json::from_str(r#"{"provider":"coreml"}"#).unwrap();
        assert_eq!(name.provider, Some(AsrProvider::Coreml));
        let automatic: Holder = serde_json::from_str(r#"{"provider":null}"#).unwrap();
        assert_eq!(automatic.provider, None);
        let legacy: Holder = serde_json::from_str(r#"{"provider":5}"#).unwrap();
        assert_eq!(legacy.provider, Some(AsrProvider::Api));
        let old_auto: Holder = serde_json::from_str(r#"{"provider":0}"#).unwrap();
        assert_eq!(old_auto.provider, None);
        assert_eq!(
            serde_json::to_string(&name).unwrap(),
            r#"{"provider":"coreml"}"#
        );
    }
}
