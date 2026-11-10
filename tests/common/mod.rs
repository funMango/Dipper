#![allow(dead_code)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use rusqlite::Connection;
use sha2::{Digest, Sha256};

/// Shared helpers for integration tests. Not every test crate calls every helper.
pub fn make_sqlite(dir: &Path, file: &str, tables: &[&str]) -> PathBuf {
    let path = dir.join(file);
    let conn = Connection::open(&path).expect("create sqlite test source");
    for table in tables {
        match *table {
            "ORDERS" => {
                conn.execute(
                    "CREATE TABLE ORDERS (
                        ORDER_ID INTEGER,
                        CUSTOMER_ID INTEGER,
                        STATUS TEXT,
                        AMOUNT REAL
                    )",
                    [],
                )
                .unwrap();
                conn.execute(
                    "INSERT INTO ORDERS (ORDER_ID, CUSTOMER_ID, STATUS, AMOUNT) VALUES (1, 10, 'PAID', 12.5)",
                    [],
                )
                .unwrap();
                conn.execute(
                    "INSERT INTO ORDERS (ORDER_ID, CUSTOMER_ID, STATUS, AMOUNT) VALUES (2, 11, 'SHIPPED', 7.0)",
                    [],
                )
                .unwrap();
            }
            "CUSTOMERS" => {
                conn.execute(
                    "CREATE TABLE CUSTOMERS (CUSTOMER_ID INTEGER, REGION TEXT)",
                    [],
                )
                .unwrap();
                conn.execute(
                    "INSERT INTO CUSTOMERS (CUSTOMER_ID, REGION) VALUES (10, 'WEST')",
                    [],
                )
                .unwrap();
                conn.execute(
                    "INSERT INTO CUSTOMERS (CUSTOMER_ID, REGION) VALUES (11, 'EAST')",
                    [],
                )
                .unwrap();
            }
            other => panic!("unknown test table {other}"),
        }
    }
    path
}

/// Build ORDERS (10_000), CUSTOMERS (20_000) and ITEMS (30_000) for chunked reads.
pub fn make_bulk_sqlite(dir: &Path, file: &str) -> PathBuf {
    let path = dir.join(file);
    let mut conn = Connection::open(&path).expect("create bulk sqlite source");
    conn.execute_batch(
        "CREATE TABLE ORDERS (ORDER_ID INTEGER, STATUS TEXT, AMOUNT REAL);
         CREATE TABLE CUSTOMERS (CUSTOMER_ID INTEGER, REGION TEXT);
         CREATE TABLE ITEMS (ITEM_ID INTEGER, ORDER_ID INTEGER, QTY INTEGER);",
    )
    .unwrap();
    insert_series(
        &mut conn,
        "INSERT INTO ORDERS (ORDER_ID, STATUS, AMOUNT) VALUES (?1, ?2, ?3)",
        10_000,
        |index, stmt| {
            let status = if index % 2 == 0 { "PAID" } else { "SHIPPED" };
            stmt.execute(rusqlite::params![index, status, index as f64])
                .unwrap();
        },
    );
    insert_series(
        &mut conn,
        "INSERT INTO CUSTOMERS (CUSTOMER_ID, REGION) VALUES (?1, ?2)",
        20_000,
        |index, stmt| {
            stmt.execute(rusqlite::params![index, format!("R{index}")])
                .unwrap();
        },
    );
    insert_series(
        &mut conn,
        "INSERT INTO ITEMS (ITEM_ID, ORDER_ID, QTY) VALUES (?1, ?2, ?3)",
        30_000,
        |index, stmt| {
            stmt.execute(rusqlite::params![index, index % 10_000, index % 7])
                .unwrap();
        },
    );
    path
}

/// `EVENTS.TS` holds ISO timestamps so period filters can be counted exactly.
pub fn make_events_sqlite(dir: &Path, file: &str) -> PathBuf {
    let path = dir.join(file);
    let conn = Connection::open(&path).expect("create events sqlite source");
    conn.execute_batch(
        "CREATE TABLE EVENTS (ID INTEGER PRIMARY KEY, TS TEXT);
         INSERT INTO EVENTS (ID, TS) VALUES
           (1, '2026-08-20 23:00:00'),
           (2, '2026-08-21 00:00:00'),
           (3, '2026-08-21 08:00:00'),
           (4, '2026-08-21 08:00:00'),
           (5, '2026-08-21 15:00:00'),
           (6, '2026-08-22 00:00:00'),
           (7, '2026-09-20 12:00:00'),
           (8, '2026-09-21 00:00:00');",
    )
    .unwrap();
    path
}

fn insert_series(
    conn: &mut Connection,
    sql: &str,
    count: i64,
    mut write: impl FnMut(i64, &mut rusqlite::Statement<'_>),
) {
    let tx = conn.transaction().unwrap();
    {
        let mut stmt = tx.prepare(sql).unwrap();
        for index in 1..=count {
            write(index, &mut stmt);
        }
    }
    tx.commit().unwrap();
}

pub fn write_config(dir: &Path, toml_text: &str) -> PathBuf {
    let path = dir.join("dipper.toml");
    std::fs::write(&path, toml_text).unwrap();
    path
}

pub fn dipper() -> Command {
    Command::cargo_bin("dipper").unwrap()
}

pub fn sha256_file(path: &Path) -> String {
    let bytes = std::fs::read(path).unwrap();
    format!("{:x}", Sha256::digest(bytes))
}

pub fn list_dir(dir: &Path) -> BTreeSet<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect()
}
