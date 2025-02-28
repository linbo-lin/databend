// Copyright 2023 Databend Cloud
//
// Licensed under the Elastic License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     https://www.elastic.co/licensing/elastic-license
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::sync::Arc;
use std::time::Instant;

use common_catalog::table::Table;
use common_exception::Result;
use common_storages_fuse::FuseTable;
use futures_util::TryStreamExt;
use opendal::EntryMode;
use opendal::Metakey;
use tracing::info;

#[async_backtrace::framed]
async fn do_vacuum_drop_table(
    table: Arc<dyn Table>,
    dry_run_limit: Option<usize>,
) -> Result<Option<Vec<(String, String)>>> {
    // only operate fuse table
    if table.engine() != "FUSE" {
        info!(
            "ignore table {} not of FUSE engine, table engine {}",
            table.get_table_info().name,
            table.engine()
        );
        return Ok(None);
    }
    let table_info = table.get_table_info();
    // storage_params is_some means it is an external table, ignore
    if table_info.meta.storage_params.is_some() {
        info!("ignore external table {}", table.get_table_info().name);
        return Ok(None);
    }
    let fuse_table = FuseTable::try_from_table(table.as_ref())?;

    let operator = fuse_table.get_operator_ref();

    let dir = format!("{}/", FuseTable::parse_storage_prefix(table_info)?);
    info!("vacuum drop table {:?} dir {:?}", table.name(), dir);
    let start = Instant::now();

    let ret = match dry_run_limit {
        None => {
            let _ = operator.remove_all(&dir).await;

            Ok(None)
        }
        Some(dry_run_limit) => {
            let mut ds = operator.list_with(&dir).delimiter("").await?;
            let mut list_files = Vec::new();
            while let Some(de) = ds.try_next().await? {
                let meta = operator.metadata(&de, Metakey::Mode).await?;
                if EntryMode::FILE == meta.mode() {
                    list_files.push((fuse_table.name().to_string(), de.name().to_string()));
                    if list_files.len() >= dry_run_limit {
                        break;
                    }
                }
            }

            Ok(Some(list_files))
        }
    };

    info!(
        "vacuum drop table {:?} dir {:?}, cost:{} sec",
        table.name(),
        dir,
        start.elapsed().as_secs()
    );
    ret
}

#[async_backtrace::framed]
pub async fn do_vacuum_drop_tables(
    tables: Vec<Arc<dyn Table>>,
    dry_run_limit: Option<usize>,
) -> Result<Option<Vec<(String, String)>>> {
    let start = Instant::now();
    let tables_len = tables.len();
    info!("do_vacuum_drop_tables {} tables", tables_len);
    let mut list_files = Vec::new();
    let mut left_limit = dry_run_limit;
    for table in tables {
        let ret = do_vacuum_drop_table(table, left_limit).await?;
        if let Some(ret) = ret {
            list_files.extend(ret);
            if list_files.len() >= dry_run_limit.unwrap() {
                info!(
                    "do_vacuum_drop_tables {} tables, cost:{} sec",
                    tables_len,
                    start.elapsed().as_secs()
                );
                return Ok(Some(list_files));
            } else {
                left_limit = Some(dry_run_limit.unwrap() - list_files.len());
            }
        }
    }
    info!(
        "do_vacuum_drop_tables {} tables, cost:{} sec",
        tables_len,
        start.elapsed().as_secs()
    );

    Ok(if dry_run_limit.is_some() {
        Some(list_files)
    } else {
        None
    })
}
