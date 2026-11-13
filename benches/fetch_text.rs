//-----------------------------------------------------------------------------
// Copyright (c) 2026, Oracle and/or its affiliates.
//
// This software is dual-licensed to you under the Universal Permissive License
// (UPL) 1.0 as shown at https://oss.oracle.com/licenses/upl and Apache License
// 2.0 as shown at http://www.apache.org/licenses/LICENSE-2.0. You may choose
// either license.
//
// If you elect to accept the software under the Apache License, Version 2.0,
// the following applies:
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//    https://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.
//-----------------------------------------------------------------------------

//-----------------------------------------------------------------------------
// Fetches 100,000 rows of six VARCHAR2 columns (16 to 512 characters, with
// non-ASCII text and NULLs) and takes each value as an owned String.
//
// Uses the RSO_TEST_* environment variables described in tests/README.md.
//
//   cargo bench --bench fetch_text -- --save-baseline main
//   git cherry-pick <patch>
//   cargo bench --bench fetch_text -- --baseline main
//
// Criterion measures elapsed time. Client CPU per query and peak RSS come from
// a separate run of PROBE_QUERIES queries, read from /proc (Linux only).
//-----------------------------------------------------------------------------

use std::hint::black_box;
use std::time::Duration;

use criterion::{
    BenchmarkId, Criterion, SamplingMode, Throughput, criterion_group,
    criterion_main,
};

const TABLE: &str = "bench_fetch_text";
const ROWS: usize = 100_000;
const WIDTHS: [usize; 6] = [16, 32, 64, 128, 256, 512];
const FETCH_SIZES: [u32; 2] = [1024, 10000];
const PROBE_QUERIES: u32 = 10;

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set"))
}

fn create_table(conn: &oracledb::Connection) -> Result<(), oracledb::Error> {
    let columns: Vec<String> = WIDTHS
        .iter()
        .enumerate()
        .map(|(i, width)| {
            format!(
                "case when mod(level + {i}, 17) = 0 then null else \
                 cast(rpad('row-' || level || '-é-', {width}, 'abcdef') \
                 as varchar2({})) end c{i}",
                width * 2
            )
        })
        .collect();
    let sql = format!(
        "create table {TABLE} as select {} from dual connect by level <= {ROWS}",
        columns.join(", ")
    );
    match conn.execute(&sql, &[]) {
        Err(err)
            if matches!(err.kind(), oracledb::ErrorKind::DbError(e)
                if e.code() == 955) => {}
        result => {
            result?;
        }
    }
    Ok(())
}

fn fetch(
    conn: &oracledb::Connection,
    fetch_size: u32,
) -> Result<usize, oracledb::Error> {
    let sql = format!("select * from {TABLE}");
    let stmt = conn
        .statement(&sql)?
        .fetch_array_size(fetch_size)
        .prefetch_rows(0)
        .exclude_from_cache()
        .build()?;
    let mut count = 0;
    for row in stmt.query(&[])? {
        let mut row = row?;
        for i in 0..WIDTHS.len() {
            black_box(row.take::<Option<String>>(i)?);
        }
        count += 1;
    }
    Ok(count)
}

// Writing 5 to clear_refs resets VmHWM, so each fetch size gets its own peak.
fn reset_peak_rss() {
    let _ = std::fs::write("/proc/self/clear_refs", "5");
}

fn peak_rss_mib() -> Option<f64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("VmHWM:"))?;
    let kib: f64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kib / 1024.0)
}

// Process user + system CPU in milliseconds (USER_HZ is 100 on Linux).
fn cpu_ms() -> Option<f64> {
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    let mut fields = stat.rsplit_once(')')?.1.split_whitespace().skip(11);
    let utime: f64 = fields.next()?.parse().ok()?;
    let stime: f64 = fields.next()?.parse().ok()?;
    Some((utime + stime) * 10.0)
}

fn fmt(value: Option<f64>) -> String {
    value.map_or_else(|| "n/a".to_string(), |v| format!("{v:.2}"))
}

fn fetch_text(c: &mut Criterion) {
    let config = oracledb::Config::default()
        .set_credentials(
            &env("RSO_TEST_MAIN_USER"),
            &env("RSO_TEST_MAIN_PASSWORD"),
        )
        .set_connect_string(&env("RSO_TEST_CONNECT_STRING"))
        .unwrap();
    let conn = oracledb::connect(config).unwrap();
    create_table(&conn).unwrap();
    assert_eq!(fetch(&conn, 1024).unwrap(), ROWS);

    // Runs before Criterion, whose sampling and analysis would raise CPU and RSS.
    for fetch_size in FETCH_SIZES {
        reset_peak_rss();
        let cpu_start = cpu_ms();
        for _ in 0..PROBE_QUERIES {
            assert_eq!(fetch(&conn, fetch_size).unwrap(), ROWS);
        }
        let cpu = cpu_ms()
            .zip(cpu_start)
            .map(|(end, start)| (end - start) / f64::from(PROBE_QUERIES));
        println!(
            "fetch_text/{fetch_size}: client CPU {} ms/query, peak RSS {} MiB",
            fmt(cpu),
            fmt(peak_rss_mib()),
        );
    }

    let mut group = c.benchmark_group("fetch_text");
    group
        .sampling_mode(SamplingMode::Flat)
        .sample_size(20)
        .measurement_time(Duration::from_secs(10))
        .throughput(Throughput::Elements(ROWS as u64));
    for fetch_size in FETCH_SIZES {
        group.bench_with_input(
            BenchmarkId::from_parameter(fetch_size),
            &fetch_size,
            |b, &fetch_size| {
                b.iter(|| assert_eq!(fetch(&conn, fetch_size).unwrap(), ROWS))
            },
        );
    }
    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default().without_plots();
    targets = fetch_text
}
criterion_main!(benches);
