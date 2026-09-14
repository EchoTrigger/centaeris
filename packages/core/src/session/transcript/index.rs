use std::cell::Cell;
use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::{Deserialize, Serialize};

use super::{
    parse_decimal_u64, require_identifier, TranscriptBlockV1, TranscriptOrderKeyV1,
    TranscriptPagePolicyV1, TranscriptPageReadRequestV1, TranscriptPageV1,
    TranscriptResumeCursorV1, TRANSCRIPT_PAGE_RESUME_CURSOR_MAX_COUNT, TRANSCRIPT_PAGE_SCHEMA_V1,
    TRANSCRIPT_PROJECTION_VERSION_V1,
};

#[derive(Clone, Debug)]
struct VersionedBlock {
    applied_sequence: u64,
    block: TranscriptBlockV1,
}

#[derive(Clone, Debug)]
struct VersionedResumeCursor {
    source_high_water: u64,
    cursor: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TranscriptPageCursorV1 {
    schema: String,
    session_id: String,
    projection_version: String,
    projection_generation: String,
    source_high_water: String,
    before: TranscriptOrderKeyV1,
}

struct CountingIterator<I> {
    inner: I,
    count: Rc<Cell<usize>>,
}

impl<I: Iterator> Iterator for CountingIterator<I> {
    type Item = I::Item;

    fn next(&mut self) -> Option<Self::Item> {
        let next = self.inner.next();
        if next.is_some() {
            self.count.set(self.count.get().saturating_add(1));
        }
        next
    }
}

pub fn transcript_page_before_order(
    request: &TranscriptPageReadRequestV1,
) -> Result<Option<(u64, u32)>, String> {
    request.validate()?;
    request
        .older_cursor
        .as_deref()
        .map(|cursor| {
            decode_bound_cursor(
                cursor,
                request.session_id.as_str(),
                request.projection_generation.as_str(),
                parse_decimal_u64(
                    request.source_high_water.as_str(),
                    "transcript page request sourceHighWater",
                )?,
            )
            .and_then(|key| Ok((key.source_sequence_value()?, key.ordinal)))
        })
        .transpose()
}

pub fn assemble_transcript_page_from_newest<I>(
    request: &TranscriptPageReadRequestV1,
    candidates: I,
    resume_cursors: Vec<TranscriptResumeCursorV1>,
) -> Result<(TranscriptPageV1, usize), String>
where
    I: IntoIterator<Item = Result<TranscriptBlockV1, String>>,
{
    request.validate()?;
    transcript_page_before_order(request)?;
    let source_high_water = parse_decimal_u64(
        request.source_high_water.as_str(),
        "transcript page request sourceHighWater",
    )?;
    let count = Rc::new(Cell::new(0usize));
    let mut eligible = CountingIterator {
        inner: candidates.into_iter(),
        count: Rc::clone(&count),
    }
    .peekable();
    let mut newest_first = Vec::new();
    let mut inline_bytes = 0usize;
    let mut has_older = false;
    loop {
        if newest_first.len() >= request.policy.max_blocks {
            has_older = match eligible.next() {
                Some(Ok(_)) => true,
                Some(Err(error)) => return Err(error),
                None => false,
            };
            break;
        }
        let Some(candidate) = eligible.next() else {
            break;
        };
        let block = candidate?;
        block.validate()?;
        if block.order_key.source_sequence_value()? > source_high_water {
            return Err("transcript page candidate is after sourceHighWater".to_string());
        }
        let next_inline_bytes = inline_bytes.saturating_add(block.body.inline_content_bytes());
        if next_inline_bytes > request.policy.max_inline_content_bytes {
            has_older = true;
            break;
        }
        newest_first.push(block);
        inline_bytes = next_inline_bytes;
        let more_blocks = match eligible.peek() {
            Some(Ok(_)) => true,
            Some(Err(error)) => return Err(error.clone()),
            None => false,
        };
        let provisional = build_bound_page(
            request,
            source_high_water,
            &newest_first,
            more_blocks,
            resume_cursors.clone(),
        )?;
        if serde_json::to_vec(&provisional)
            .map_err(|error| format!("serialize transcript page failed: {error}"))?
            .len()
            > request.policy.max_serialized_bytes
        {
            newest_first.pop();
            has_older = true;
            break;
        }
        if !more_blocks {
            break;
        }
    }
    if newest_first.is_empty() && has_older {
        return Err("transcript page policy cannot fit one valid block".to_string());
    }
    let page = build_bound_page(
        request,
        source_high_water,
        &newest_first,
        has_older,
        resume_cursors,
    )?;
    page.validate(request.policy)?;
    Ok((page, count.get()))
}

fn build_bound_page(
    request: &TranscriptPageReadRequestV1,
    source_high_water: u64,
    newest_first: &[TranscriptBlockV1],
    has_older: bool,
    resume_cursors: Vec<TranscriptResumeCursorV1>,
) -> Result<TranscriptPageV1, String> {
    let older_cursor = if has_older {
        newest_first
            .last()
            .map(|block| {
                encode_bound_cursor(
                    request.session_id.as_str(),
                    request.projection_generation.as_str(),
                    source_high_water,
                    block.order_key.clone(),
                )
            })
            .transpose()?
    } else {
        None
    };
    let mut blocks = newest_first.to_vec();
    blocks.reverse();
    Ok(TranscriptPageV1 {
        schema: TRANSCRIPT_PAGE_SCHEMA_V1.to_string(),
        session_id: request.session_id.clone(),
        projection_version: TRANSCRIPT_PROJECTION_VERSION_V1.to_string(),
        projection_generation: request.projection_generation.clone(),
        source_high_water: request.source_high_water.clone(),
        blocks,
        older_cursor,
        has_older,
        resume_cursors,
    })
}

#[derive(Clone, Debug)]
pub struct TranscriptBlockIndexV1 {
    session_id: String,
    projection_generation: String,
    source_high_water: u64,
    block_order: BTreeMap<(u64, u32), String>,
    order_by_block_id: HashMap<String, (u64, u32)>,
    versions_by_block_id: HashMap<String, Vec<VersionedBlock>>,
    resume_cursors_by_stream: BTreeMap<String, Vec<VersionedResumeCursor>>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TranscriptPageQueryWorkV1 {
    pub order_keys_visited: usize,
    pub block_version_lookups: usize,
    pub returned_blocks: usize,
    pub raw_event_visits: usize,
}

impl TranscriptBlockIndexV1 {
    pub fn new(session_id: String, projection_generation: String) -> Result<Self, String> {
        require_identifier(session_id.as_str(), "transcript index sessionId")?;
        require_identifier(
            projection_generation.as_str(),
            "transcript index projectionGeneration",
        )?;
        Ok(Self {
            session_id,
            projection_generation,
            source_high_water: 0,
            block_order: BTreeMap::new(),
            order_by_block_id: HashMap::new(),
            versions_by_block_id: HashMap::new(),
            resume_cursors_by_stream: BTreeMap::new(),
        })
    }

    pub fn projection_generation(&self) -> &str {
        self.projection_generation.as_str()
    }

    pub fn apply_committed(
        &mut self,
        applied_sequence: u64,
        block: TranscriptBlockV1,
    ) -> Result<(), String> {
        block.validate()?;
        if let Some(existing) = self
            .versions_by_block_id
            .get(block.block_id.as_str())
            .and_then(|versions| {
                versions
                    .iter()
                    .find(|item| item.applied_sequence == applied_sequence)
            })
        {
            if existing.block == block {
                return Ok(());
            }
            return Err(
                "transcript block applied sequence is already bound to another revision"
                    .to_string(),
            );
        }
        if applied_sequence == 0 || applied_sequence < self.source_high_water {
            return Err("transcript committed sequence is not monotonic".to_string());
        }
        let source_sequence = block.order_key.source_sequence_value()?;
        if source_sequence > applied_sequence {
            return Err("transcript block orderKey is after its applied sequence".to_string());
        }
        let order = (source_sequence, block.order_key.ordinal);
        let order_is_new = if let Some(existing_order) =
            self.order_by_block_id.get(block.block_id.as_str())
        {
            if *existing_order != order {
                return Err("transcript block orderKey changed across revisions".to_string());
            }
            false
        } else {
            match self.block_order.entry(order) {
                Entry::Occupied(_) => {
                    return Err("transcript orderKey is already bound to another block".to_string())
                }
                Entry::Vacant(_) => true,
            }
        };

        let block_revision =
            parse_decimal_u64(block.block_revision.as_str(), "transcript blockRevision")?;
        let previous = self
            .versions_by_block_id
            .get(block.block_id.as_str())
            .and_then(|versions| versions.last());
        if let Some(previous) = previous {
            if previous.applied_sequence >= applied_sequence {
                return Err("transcript block applied sequence is not increasing".to_string());
            }
            let previous_revision = parse_decimal_u64(
                previous.block.block_revision.as_str(),
                "transcript previous blockRevision",
            )?;
            if block_revision <= previous_revision {
                return Err("transcript blockRevision is not increasing".to_string());
            }
        } else if applied_sequence != source_sequence {
            return Err(
                "transcript block first revision must be applied at its order sourceSequence"
                    .to_string(),
            );
        }

        if order_is_new {
            self.block_order.insert(order, block.block_id.clone());
            self.order_by_block_id.insert(block.block_id.clone(), order);
        }
        self.versions_by_block_id
            .entry(block.block_id.clone())
            .or_default()
            .push(VersionedBlock {
                applied_sequence,
                block,
            });
        self.source_high_water = applied_sequence;
        Ok(())
    }

    pub fn advance_source_high_water(&mut self, source_high_water: u64) -> Result<(), String> {
        if source_high_water < self.source_high_water {
            return Err("transcript sourceHighWater cannot move backward".to_string());
        }
        self.source_high_water = source_high_water;
        Ok(())
    }

    pub fn record_resume_cursor(
        &mut self,
        source_high_water: u64,
        stream_id: String,
        cursor: String,
    ) -> Result<(), String> {
        require_identifier(stream_id.as_str(), "transcript resume streamId")?;
        require_identifier(cursor.as_str(), "transcript resume cursor")?;
        if source_high_water == 0 || source_high_water > self.source_high_water {
            return Err("transcript resume cursor waterline is not projected".to_string());
        }
        if !self
            .resume_cursors_by_stream
            .contains_key(stream_id.as_str())
            && self.resume_cursors_by_stream.len() >= TRANSCRIPT_PAGE_RESUME_CURSOR_MAX_COUNT
        {
            return Err("transcript index exceeds maximum resume stream count".to_string());
        }
        let versions = self.resume_cursors_by_stream.entry(stream_id).or_default();
        if let Some(previous) = versions.last() {
            if previous.source_high_water == source_high_water && previous.cursor == cursor {
                return Ok(());
            }
            if previous.source_high_water >= source_high_water {
                return Err("transcript resume cursor waterline is not increasing".to_string());
            }
        }
        versions.push(VersionedResumeCursor {
            source_high_water,
            cursor,
        });
        Ok(())
    }

    pub fn page_at(
        &self,
        source_high_water: u64,
        older_cursor: Option<&str>,
        policy: TranscriptPagePolicyV1,
    ) -> Result<TranscriptPageV1, String> {
        self.page_at_with_work(source_high_water, older_cursor, policy)
            .map(|(page, _)| page)
    }

    pub fn page_at_with_work(
        &self,
        source_high_water: u64,
        older_cursor: Option<&str>,
        policy: TranscriptPagePolicyV1,
    ) -> Result<(TranscriptPageV1, TranscriptPageQueryWorkV1), String> {
        let policy = policy.validate()?;
        if source_high_water > self.source_high_water {
            return Err("transcript page sourceHighWater is not fully projected".to_string());
        }
        let before = older_cursor
            .map(|cursor| self.decode_cursor(cursor, source_high_water))
            .transpose()?;
        let before_order = match before.as_ref() {
            Some(key) => Some((key.source_sequence_value()?, key.ordinal)),
            None => None,
        };

        let order_keys_visited = Cell::new(0usize);
        let block_version_lookups = Cell::new(0usize);
        let range = match before_order {
            Some(before) => self.block_order.range(..before),
            None => self.block_order.range(..=(source_high_water, u32::MAX)),
        };
        let mut eligible = range
            .rev()
            .inspect(|_| order_keys_visited.set(order_keys_visited.get().saturating_add(1)))
            .filter_map(|(_, block_id)| {
                block_version_lookups.set(block_version_lookups.get().saturating_add(1));
                self.versions_by_block_id
                    .get(block_id)
                    .and_then(|versions| {
                        let boundary = versions
                            .partition_point(|item| item.applied_sequence <= source_high_water);
                        boundary.checked_sub(1).map(|index| &versions[index])
                    })
                    .map(|item| item.block.clone())
            })
            .peekable();

        let mut newest_first = Vec::new();
        let mut inline_bytes = 0usize;
        let mut has_older = false;
        loop {
            if newest_first.len() >= policy.max_blocks {
                has_older = eligible.peek().is_some();
                break;
            }
            let Some(block) = eligible.next() else {
                break;
            };
            let next_inline_bytes = inline_bytes.saturating_add(block.body.inline_content_bytes());
            if next_inline_bytes > policy.max_inline_content_bytes {
                has_older = true;
                break;
            }
            newest_first.push(block);
            inline_bytes = next_inline_bytes;
            let more_blocks = eligible.peek().is_some();
            let provisional = self.build_page(source_high_water, &newest_first, more_blocks)?;
            if serde_json::to_vec(&provisional)
                .map_err(|error| format!("serialize transcript page failed: {error}"))?
                .len()
                > policy.max_serialized_bytes
            {
                newest_first.pop();
                has_older = true;
                break;
            }
            if !more_blocks {
                break;
            }
        }
        if newest_first.is_empty() && has_older {
            return Err("transcript page policy cannot fit one valid block".to_string());
        }
        let page = self.build_page(source_high_water, &newest_first, has_older)?;
        page.validate(policy)?;
        Ok((
            page,
            TranscriptPageQueryWorkV1 {
                order_keys_visited: order_keys_visited.get(),
                block_version_lookups: block_version_lookups.get(),
                returned_blocks: newest_first.len(),
                raw_event_visits: 0,
            },
        ))
    }

    fn build_page(
        &self,
        source_high_water: u64,
        newest_first: &[TranscriptBlockV1],
        has_older: bool,
    ) -> Result<TranscriptPageV1, String> {
        let older_cursor = if has_older {
            newest_first
                .last()
                .map(|block| self.encode_cursor(source_high_water, block.order_key.clone()))
                .transpose()?
        } else {
            None
        };
        let mut blocks = newest_first.to_vec();
        blocks.reverse();
        let resume_cursors = self
            .resume_cursors_by_stream
            .iter()
            .filter_map(|(stream_id, versions)| {
                versions
                    .iter()
                    .rev()
                    .find(|item| item.source_high_water <= source_high_water)
                    .map(|item| TranscriptResumeCursorV1 {
                        stream_id: stream_id.clone(),
                        cursor: item.cursor.clone(),
                    })
            })
            .collect();
        Ok(TranscriptPageV1 {
            schema: TRANSCRIPT_PAGE_SCHEMA_V1.to_string(),
            session_id: self.session_id.clone(),
            projection_version: TRANSCRIPT_PROJECTION_VERSION_V1.to_string(),
            projection_generation: self.projection_generation.clone(),
            source_high_water: source_high_water.to_string(),
            blocks,
            older_cursor,
            has_older,
            resume_cursors,
        })
    }

    fn encode_cursor(
        &self,
        source_high_water: u64,
        before: TranscriptOrderKeyV1,
    ) -> Result<String, String> {
        encode_bound_cursor(
            self.session_id.as_str(),
            self.projection_generation.as_str(),
            source_high_water,
            before,
        )
    }

    fn decode_cursor(
        &self,
        cursor: &str,
        source_high_water: u64,
    ) -> Result<TranscriptOrderKeyV1, String> {
        decode_bound_cursor(
            cursor,
            self.session_id.as_str(),
            self.projection_generation.as_str(),
            source_high_water,
        )
    }
}

fn encode_bound_cursor(
    session_id: &str,
    projection_generation: &str,
    source_high_water: u64,
    before: TranscriptOrderKeyV1,
) -> Result<String, String> {
    let bytes = serde_json::to_vec(&TranscriptPageCursorV1 {
        schema: TRANSCRIPT_PAGE_SCHEMA_V1.to_string(),
        session_id: session_id.to_string(),
        projection_version: TRANSCRIPT_PROJECTION_VERSION_V1.to_string(),
        projection_generation: projection_generation.to_string(),
        source_high_water: source_high_water.to_string(),
        before,
    })
    .map_err(|error| format!("serialize transcript page cursor failed: {error}"))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn decode_bound_cursor(
    cursor: &str,
    session_id: &str,
    projection_generation: &str,
    source_high_water: u64,
) -> Result<TranscriptOrderKeyV1, String> {
    let bytes = URL_SAFE_NO_PAD
        .decode(cursor)
        .map_err(|_| "transcript page cursor is invalid".to_string())?;
    let decoded: TranscriptPageCursorV1 = serde_json::from_slice(bytes.as_slice())
        .map_err(|_| "transcript page cursor is invalid".to_string())?;
    if decoded.schema != TRANSCRIPT_PAGE_SCHEMA_V1
        || decoded.session_id != session_id
        || decoded.projection_version != TRANSCRIPT_PROJECTION_VERSION_V1
        || decoded.projection_generation != projection_generation
        || decoded.source_high_water != source_high_water.to_string()
    {
        return Err("transcript page cursor binding mismatch".to_string());
    }
    decoded.before.source_sequence_value()?;
    Ok(decoded.before)
}
