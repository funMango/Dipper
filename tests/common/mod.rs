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
