// Copyright 2021 Datafuse Labs
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::sync::Arc;

use common_catalog::plan::Projection;
use common_catalog::table::CompactTarget;
use common_exception::Result;
use common_pipeline_core::processors::processor::ProcessorPtr;
use common_pipeline_transforms::processors::transforms::AsyncAccumulatingTransformer;
use storages_common_table_meta::meta::TableSnapshot;

use crate::operations::common::CommitSink;
use crate::operations::common::MutationGenerator;
use crate::operations::common::TransformSerializeBlock;
use crate::operations::mutation::BlockCompactMutator;
use crate::operations::mutation::CompactAggregator;
use crate::operations::mutation::CompactSource;
use crate::operations::mutation::SegmentCompactMutator;
use crate::pipelines::Pipeline;
use crate::FuseTable;
use crate::Table;
use crate::TableContext;
use crate::DEFAULT_BLOCK_PER_SEGMENT;
use crate::FUSE_OPT_KEY_BLOCK_PER_SEGMENT;

#[derive(Clone)]
pub struct CompactOptions {
    // the snapshot that compactor working on, it never changed during phases compaction.
    pub base_snapshot: Arc<TableSnapshot>,
    pub block_per_seg: usize,
    pub limit: Option<usize>,
}

impl FuseTable {
    #[async_backtrace::framed]
    pub(crate) async fn do_compact(
        &self,
        ctx: Arc<dyn TableContext>,
        target: CompactTarget,
        limit: Option<usize>,
        pipeline: &mut Pipeline,
    ) -> Result<()> {
        let snapshot_opt = self.read_table_snapshot().await?;
        let base_snapshot = if let Some(val) = snapshot_opt {
            val
        } else {
            // no snapshot, no compaction.
            return Ok(());
        };

        if base_snapshot.summary.block_count <= 1 {
            return Ok(());
        }

        let block_per_seg =
            self.get_option(FUSE_OPT_KEY_BLOCK_PER_SEGMENT, DEFAULT_BLOCK_PER_SEGMENT);

        let compact_params = CompactOptions {
            base_snapshot,
            block_per_seg,
            limit,
        };

        match target {
            CompactTarget::Blocks => self.compact_blocks(ctx, pipeline, compact_params).await,
            CompactTarget::Segments => self.compact_segments(ctx, pipeline, compact_params).await,
        }
    }

    #[async_backtrace::framed]
    async fn compact_segments(
        &self,
        ctx: Arc<dyn TableContext>,
        _pipeline: &mut Pipeline,
        options: CompactOptions,
    ) -> Result<()> {
        let mut segment_mutator = SegmentCompactMutator::try_create(
            ctx.clone(),
            options,
            self.meta_location_generator().clone(),
            self.operator.clone(),
        )?;

        if !segment_mutator.target_select().await? {
            return Ok(());
        }

        segment_mutator.try_commit(Arc::new(self.clone())).await
    }

    /// The flow of Pipeline is as follows:
    /// +-------------+      +-----------------------+
    /// |CompactSource| ---> |TransformSerializeBlock|   ------
    /// +-------------+      +-----------------------+         |      +-----------------+      +----------+
    /// |     ...     | ---> |          ...          |   ...   | ---> |CompactAggregator| ---> |CommitSink|
    /// +-------------+      +-----------------------+         |      +-----------------+      +----------+
    /// |CompactSource| ---> |TransformSerializeBlock|   ------
    /// +-------------+      +-----------------------+
    #[async_backtrace::framed]
    async fn compact_blocks(
        &self,
        ctx: Arc<dyn TableContext>,
        pipeline: &mut Pipeline,
        options: CompactOptions,
    ) -> Result<()> {
        let thresholds = self.get_block_thresholds();

        let mut mutator = BlockCompactMutator::new(
            ctx.clone(),
            thresholds,
            options,
            self.operator.clone(),
            self.cluster_key_meta.as_ref().map(|k| k.0),
        );
        mutator.target_select().await?;
        if mutator.compact_tasks.is_empty() {
            return Ok(());
        }

        // Status.
        ctx.set_status_info("compact: begin to run compact tasks");
        ctx.set_partitions(mutator.compact_tasks.clone())?;

        let all_column_indices = self.all_column_indices();
        let projection = Projection::Columns(all_column_indices);
        let block_reader = self.create_block_reader(projection, false, ctx.clone())?;
        let max_threads = std::cmp::min(
            ctx.get_settings().get_max_threads()? as usize,
            mutator.compact_tasks.len(),
        );
        // Add source pipe.
        pipeline.add_source(
            |output| {
                CompactSource::try_create(
                    ctx.clone(),
                    self.storage_format,
                    block_reader.clone(),
                    output,
                )
            },
            max_threads,
        )?;

        let block_thresholds = self.get_block_thresholds();
        // sort
        let cluster_stats_gen =
            self.cluster_gen_for_append(ctx.clone(), pipeline, block_thresholds)?;

        pipeline.add_transform(
            |input: Arc<common_pipeline_core::processors::port::InputPort>, output| {
                let proc = TransformSerializeBlock::try_create(
                    ctx.clone(),
                    input,
                    output,
                    self,
                    cluster_stats_gen.clone(),
                )?;
                proc.into_processor()
            },
        )?;

        pipeline.try_resize(1)?;

        pipeline.add_transform(|input, output| {
            let compact_aggregator = CompactAggregator::new(
                self.operator.clone(),
                self.meta_location_generator().clone(),
                mutator.clone(),
            );
            Ok(ProcessorPtr::create(AsyncAccumulatingTransformer::create(
                input,
                output,
                compact_aggregator,
            )))
        })?;

        let snapshot_gen = MutationGenerator::new(mutator.compact_params.base_snapshot);
        pipeline.add_sink(|input| {
            CommitSink::try_create(
                self,
                ctx.clone(),
                None,
                snapshot_gen.clone(),
                input,
                None,
                true,
            )
        })?;

        Ok(())
    }
}
