use std::collections::{BTreeMap, HashMap, HashSet};

use super::{
    parse_decimal_u64, require_identifier, TranscriptBlockV1, TranscriptPagePolicyV1,
    TranscriptPageV1, TranscriptPatchV1, TRANSCRIPT_PROJECTION_VERSION_V1,
};

#[derive(Clone, Debug)]
pub struct TranscriptViewStateV1 {
    view_epoch: String,
    session_id: String,
    projection_generation: String,
    base_high_water: u64,
    current_high_water: u64,
    loaded_block_ids: HashSet<String>,
    visible_blocks_by_id: HashMap<String, TranscriptBlockV1>,
    visible_order: BTreeMap<(u64, u32), String>,
    known_order_by_id: HashMap<String, (u64, u32)>,
    known_id_by_order: HashMap<(u64, u32), String>,
    pending_overrides: HashMap<String, TranscriptBlockV1>,
    post_base_override_ids: HashSet<String>,
    applied_cursors: BTreeMap<String, String>,
}

impl TranscriptViewStateV1 {
    pub fn open(view_epoch: String, page: TranscriptPageV1) -> Result<Self, String> {
        require_identifier(view_epoch.as_str(), "transcript viewEpoch")?;
        page.validate(TranscriptPagePolicyV1::default())?;
        let base_high_water = parse_decimal_u64(
            page.source_high_water.as_str(),
            "transcript page sourceHighWater",
        )?;
        let mut state = Self {
            view_epoch,
            session_id: page.session_id.clone(),
            projection_generation: page.projection_generation.clone(),
            base_high_water,
            current_high_water: base_high_water,
            loaded_block_ids: HashSet::new(),
            visible_blocks_by_id: HashMap::new(),
            visible_order: BTreeMap::new(),
            known_order_by_id: HashMap::new(),
            known_id_by_order: HashMap::new(),
            pending_overrides: HashMap::new(),
            post_base_override_ids: HashSet::new(),
            applied_cursors: BTreeMap::new(),
        };
        state.apply_page_inner(page)?;
        Ok(state)
    }

    pub fn view_epoch(&self) -> &str {
        self.view_epoch.as_str()
    }

    pub fn apply_page(&mut self, page: TranscriptPageV1) -> Result<(), String> {
        let mut candidate = self.clone();
        candidate.apply_page_inner(page)?;
        *self = candidate;
        Ok(())
    }

    pub fn apply_patch(&mut self, patch: TranscriptPatchV1) -> Result<(), String> {
        let mut candidate = self.clone();
        candidate.apply_patch_inner(patch)?;
        *self = candidate;
        Ok(())
    }

    pub fn visible_blocks(&self) -> Vec<&TranscriptBlockV1> {
        self.visible_order
            .values()
            .filter_map(|block_id| self.visible_blocks_by_id.get(block_id))
            .collect()
    }

    pub fn pending_override(&self, block_id: &str) -> Option<&TranscriptBlockV1> {
        self.pending_overrides.get(block_id)
    }

    pub fn applied_cursor(&self, stream_id: &str) -> Option<&str> {
        self.applied_cursors.get(stream_id).map(String::as_str)
    }

    pub fn release_loaded_history(&mut self, retained_block_ids: &HashSet<String>) -> usize {
        let removable = self
            .visible_blocks_by_id
            .values()
            .filter(|block| {
                block
                    .order_key
                    .source_sequence_value()
                    .is_ok_and(|sequence| {
                        sequence <= self.base_high_water
                            && !retained_block_ids.contains(block.block_id.as_str())
                            && !self
                                .post_base_override_ids
                                .contains(block.block_id.as_str())
                    })
            })
            .map(|block| block.block_id.clone())
            .collect::<Vec<_>>();
        for block_id in &removable {
            self.loaded_block_ids.remove(block_id.as_str());
            self.visible_blocks_by_id.remove(block_id.as_str());
            if let Some(order) = self.known_order_by_id.remove(block_id.as_str()) {
                self.visible_order.remove(&order);
                self.known_id_by_order.remove(&order);
            }
        }
        removable.len()
    }

    fn apply_page_inner(&mut self, page: TranscriptPageV1) -> Result<(), String> {
        page.validate(TranscriptPagePolicyV1::default())?;
        self.validate_identity(
            page.session_id.as_str(),
            page.projection_version.as_str(),
            page.projection_generation.as_str(),
        )?;
        let page_high_water = parse_decimal_u64(
            page.source_high_water.as_str(),
            "transcript page sourceHighWater",
        )?;
        if page_high_water != self.base_high_water {
            return Err("transcript history page waterline changed within the view".to_string());
        }
        for block in page.blocks {
            let block_id = block.block_id.clone();
            self.register_order(&block)?;
            self.loaded_block_ids.insert(block_id.clone());
            self.merge_visible(block)?;
            if let Some(override_block) = self.pending_overrides.remove(block_id.as_str()) {
                self.merge_visible(override_block)?;
            }
        }
        for cursor in page.resume_cursors {
            if let Some(existing) = self.applied_cursors.get(cursor.stream_id.as_str()) {
                if existing != &cursor.cursor {
                    return Err("transcript history pages disagree on a resume cursor".to_string());
                }
            } else {
                self.applied_cursors.insert(cursor.stream_id, cursor.cursor);
            }
        }
        Ok(())
    }

    fn apply_patch_inner(&mut self, patch: TranscriptPatchV1) -> Result<(), String> {
        patch.validate()?;
        self.validate_identity(
            patch.session_id.as_str(),
            patch.projection_version.as_str(),
            patch.projection_generation.as_str(),
        )?;
        let patch_high_water = parse_decimal_u64(
            patch.source_high_water.as_str(),
            "transcript patch sourceHighWater",
        )?;
        if patch_high_water < self.current_high_water {
            return Err("transcript patch sourceHighWater moved backward".to_string());
        }
        if !patch.removals.is_empty() {
            return Err("transcript block removal requires explicit view invalidation".to_string());
        }
        for block in patch.upserts {
            self.register_order(&block)?;
            let order_source = block.order_key.source_sequence_value()?;
            if self.loaded_block_ids.contains(block.block_id.as_str())
                || order_source > self.base_high_water
            {
                if order_source <= self.base_high_water {
                    self.post_base_override_ids.insert(block.block_id.clone());
                }
                self.loaded_block_ids.insert(block.block_id.clone());
                self.merge_visible(block)?;
            } else {
                self.merge_pending_override(block)?;
            }
        }
        self.current_high_water = patch_high_water;
        self.applied_cursors
            .insert(patch.stream_id, patch.applied_cursor);
        Ok(())
    }

    fn validate_identity(
        &self,
        session_id: &str,
        projection_version: &str,
        projection_generation: &str,
    ) -> Result<(), String> {
        if session_id != self.session_id
            || projection_version != TRANSCRIPT_PROJECTION_VERSION_V1
            || projection_generation != self.projection_generation
        {
            return Err("transcript view identity mismatch".to_string());
        }
        Ok(())
    }

    fn register_order(&mut self, block: &TranscriptBlockV1) -> Result<(), String> {
        let order = (
            block.order_key.source_sequence_value()?,
            block.order_key.ordinal,
        );
        if let Some(existing) = self.known_order_by_id.get(block.block_id.as_str()) {
            if existing != &order {
                return Err("transcript block orderKey changed within the view".to_string());
            }
        } else if self.known_id_by_order.contains_key(&order) {
            return Err("transcript orderKey is bound to another block in the view".to_string());
        } else {
            self.known_order_by_id.insert(block.block_id.clone(), order);
            self.known_id_by_order.insert(order, block.block_id.clone());
        }
        Ok(())
    }

    fn merge_visible(&mut self, block: TranscriptBlockV1) -> Result<(), String> {
        if let Some(existing) = self.visible_blocks_by_id.get(block.block_id.as_str()) {
            if !incoming_wins(existing, &block)? {
                return Ok(());
            }
        }
        let order = (
            block.order_key.source_sequence_value()?,
            block.order_key.ordinal,
        );
        self.visible_order.insert(order, block.block_id.clone());
        self.visible_blocks_by_id
            .insert(block.block_id.clone(), block);
        Ok(())
    }

    fn merge_pending_override(&mut self, block: TranscriptBlockV1) -> Result<(), String> {
        if let Some(existing) = self.pending_overrides.get(block.block_id.as_str()) {
            if !incoming_wins(existing, &block)? {
                return Ok(());
            }
        }
        self.pending_overrides.insert(block.block_id.clone(), block);
        Ok(())
    }
}

fn incoming_wins(
    existing: &TranscriptBlockV1,
    incoming: &TranscriptBlockV1,
) -> Result<bool, String> {
    let existing_revision = parse_decimal_u64(
        existing.block_revision.as_str(),
        "transcript existing blockRevision",
    )?;
    let incoming_revision = parse_decimal_u64(
        incoming.block_revision.as_str(),
        "transcript incoming blockRevision",
    )?;
    if incoming_revision == existing_revision {
        if incoming == existing {
            return Ok(false);
        }
        return Err("transcript block revision has conflicting content".to_string());
    }
    Ok(incoming_revision > existing_revision)
}
