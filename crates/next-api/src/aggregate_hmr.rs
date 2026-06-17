//! Aggregated HMR: one [`VersionState`] covering every chunk under a target's
//! root, so the dev server can subscribe once instead of per chunk.

use anyhow::Result;
use turbo_rcstr::RcStr;
use turbo_tasks::{FxIndexMap, ResolvedVc, TraitRef, TryJoinIterExt, Vc};
use turbo_tasks_hash::{Xxh3Hash64Hasher, encode_base64};
use turbopack_core::version::{Version, VersionedContent};

/// Per-chunk versions keyed by path. `id()` hashes sorted entries so it's
/// stable across `FxIndexMap` iteration order. Mirrors `EcmascriptDevChunkListVersion`.
#[turbo_tasks::value(serialization = "skip", shared)]
pub struct AggregateHmrVersion {
    #[turbo_tasks(trace_ignore)]
    pub by_path: FxIndexMap<RcStr, TraitRef<Box<dyn Version>>>,
}

#[turbo_tasks::value_impl]
impl Version for AggregateHmrVersion {
    #[turbo_tasks::function]
    async fn id(&self) -> Result<Vc<RcStr>> {
        let mut entries = self
            .by_path
            .iter()
            .map(|(path, version)| {
                let path = path.clone();
                let version = TraitRef::cell(version.clone());
                async move {
                    let id = version.id().owned().await?;
                    Ok::<_, anyhow::Error>((path, id))
                }
            })
            .try_join()
            .await?;
        entries.sort_by(|a, b| a.0.cmp(&b.0));

        let mut hasher = Xxh3Hash64Hasher::new();
        hasher.write_value(entries.len());
        for (path, id) in entries {
            hasher.write_value(path.as_str());
            hasher.write_value(id.as_str());
        }
        Ok(Vc::cell(encode_base64(hasher.finish()).into()))
    }
}

/// Builds an [`AggregateHmrVersion`] from a `(path, VersionedContent)` snapshot.
pub(crate) async fn build_aggregate_hmr_version(
    pairs: &[(RcStr, ResolvedVc<Box<dyn VersionedContent>>)],
) -> Result<Vc<AggregateHmrVersion>> {
    let by_path = pairs
        .iter()
        .map(|(path, content)| {
            let path = path.clone();
            let content = *content;
            async move {
                let version = content.version().into_trait_ref().await?;
                Ok::<_, anyhow::Error>((path, version))
            }
        })
        .try_join()
        .await?
        .into_iter()
        .collect();
    Ok(AggregateHmrVersion { by_path }.cell())
}

/// Unions one chunk's `EcmascriptMergedUpdate` into the combined `{entries, chunks}`.
/// Both maps are keyed by globally-unique ids, so plain insertion is safe.
pub(crate) fn merge_ecmascript_merged_update(
    combined_entries: &mut serde_json::Map<String, serde_json::Value>,
    combined_chunks: &mut serde_json::Map<String, serde_json::Value>,
    instruction: &serde_json::Value,
) {
    let Some(obj) = instruction.as_object() else {
        return;
    };
    if let Some(entries) = obj.get("entries").and_then(|v| v.as_object()) {
        for (k, v) in entries {
            combined_entries.insert(k.clone(), v.clone());
        }
    }
    if let Some(chunks) = obj.get("chunks").and_then(|v| v.as_object()) {
        for (k, v) in chunks {
            combined_chunks.insert(k.clone(), v.clone());
        }
    }
}
