#![allow(non_camel_case_types, dead_code)]

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};
use std::path::Path;
use std::ptr;

const SQLITE_OK: c_int = 0;
const SQLITE_ROW: c_int = 100;
const SQLITE_DONE: c_int = 101;
const SQLITE_OPEN_READWRITE: c_int = 0x00000002;
const SQLITE_OPEN_CREATE: c_int = 0x00000004;
const SQLITE_OPEN_FULLMUTEX: c_int = 0x00010000;

type sqlite3 = c_void;
type sqlite3_stmt = c_void;

unsafe extern "C" {
    fn sqlite3_open_v2(
        filename: *const c_char,
        ppDb: *mut *mut sqlite3,
        flags: c_int,
        zVfs: *const c_char,
    ) -> c_int;
    fn sqlite3_close(db: *mut sqlite3) -> c_int;
    fn sqlite3_exec(
        db: *mut sqlite3,
        sql: *const c_char,
        callback: *const c_void,
        arg: *const c_void,
        errmsg: *mut *mut c_char,
    ) -> c_int;
    fn sqlite3_free(ptr: *mut c_void);
    fn sqlite3_errmsg(db: *mut sqlite3) -> *const c_char;
    fn sqlite3_prepare_v2(
        db: *mut sqlite3,
        zSql: *const c_char,
        nByte: c_int,
        ppStmt: *mut *mut sqlite3_stmt,
        pzTail: *mut *const c_char,
    ) -> c_int;
    fn sqlite3_step(stmt: *mut sqlite3_stmt) -> c_int;
    fn sqlite3_finalize(stmt: *mut sqlite3_stmt) -> c_int;
    fn sqlite3_reset(stmt: *mut sqlite3_stmt) -> c_int;
    fn sqlite3_bind_text(
        stmt: *mut sqlite3_stmt,
        idx: c_int,
        text: *const c_char,
        n: c_int,
        destructor: isize,
    ) -> c_int;
    fn sqlite3_bind_int64(stmt: *mut sqlite3_stmt, idx: c_int, val: i64) -> c_int;
    fn sqlite3_column_int64(stmt: *mut sqlite3_stmt, iCol: c_int) -> i64;
    fn sqlite3_column_text(stmt: *mut sqlite3_stmt, iCol: c_int) -> *const c_char;
    fn sqlite3_changes(db: *mut sqlite3) -> c_int;
}

const SQLITE_TRANSIENT: isize = -1;

pub struct Database {
    db: *mut sqlite3,
}

unsafe impl Send for Database {}

impl Database {
    pub fn open(path: &Path) -> Result<Self, String> {
        let path_str = CString::new(path.to_str().ok_or("invalid path")?.as_bytes())
            .map_err(|e| e.to_string())?;
        let mut db: *mut sqlite3 = ptr::null_mut();
        let flags = SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE | SQLITE_OPEN_FULLMUTEX;
        let rc = unsafe { sqlite3_open_v2(path_str.as_ptr(), &mut db, flags, ptr::null()) };
        if rc != SQLITE_OK {
            let msg = if db.is_null() {
                "failed to allocate database".to_string()
            } else {
                let err = unsafe { errmsg(db) };
                unsafe { sqlite3_close(db) };
                err
            };
            return Err(msg);
        }
        Ok(Self { db })
    }

    pub fn execute_batch(&self, sql: &str) -> Result<(), String> {
        let sql_c = CString::new(sql).map_err(|e| e.to_string())?;
        let mut errmsg_ptr: *mut c_char = ptr::null_mut();
        let rc = unsafe {
            sqlite3_exec(
                self.db,
                sql_c.as_ptr(),
                ptr::null(),
                ptr::null(),
                &mut errmsg_ptr,
            )
        };
        if rc != SQLITE_OK {
            let msg = if errmsg_ptr.is_null() {
                format!("sqlite3_exec failed ({})", rc)
            } else {
                let s = unsafe { CStr::from_ptr(errmsg_ptr) }
                    .to_string_lossy()
                    .to_string();
                unsafe { sqlite3_free(errmsg_ptr as *mut c_void) };
                s
            };
            return Err(msg);
        }
        Ok(())
    }

    pub fn prepare(&self, sql: &str) -> Result<Statement, String> {
        let sql_c = CString::new(sql).map_err(|e| e.to_string())?;
        let mut stmt: *mut sqlite3_stmt = ptr::null_mut();
        let rc =
            unsafe { sqlite3_prepare_v2(self.db, sql_c.as_ptr(), -1, &mut stmt, ptr::null_mut()) };
        if rc != SQLITE_OK {
            return Err(unsafe { errmsg(self.db) });
        }
        Ok(Statement { stmt, db: self.db })
    }

    pub fn changes(&self) -> i32 {
        unsafe { sqlite3_changes(self.db) }
    }
}

impl Drop for Database {
    fn drop(&mut self) {
        if !self.db.is_null() {
            unsafe { sqlite3_close(self.db) };
        }
    }
}

pub struct Statement {
    stmt: *mut sqlite3_stmt,
    db: *mut sqlite3,
}

impl Statement {
    pub fn bind_text(&self, idx: i32, text: &str) -> Result<(), String> {
        let c = CString::new(text).map_err(|e| e.to_string())?;
        let rc =
            unsafe { sqlite3_bind_text(self.stmt, idx as c_int, c.as_ptr(), -1, SQLITE_TRANSIENT) };
        if rc != SQLITE_OK {
            return Err(unsafe { errmsg(self.db) });
        }
        Ok(())
    }

    pub fn bind_i64(&self, idx: i32, val: i64) -> Result<(), String> {
        let rc = unsafe { sqlite3_bind_int64(self.stmt, idx as c_int, val) };
        if rc != SQLITE_OK {
            return Err(unsafe { errmsg(self.db) });
        }
        Ok(())
    }

    pub fn step(&self) -> Result<bool, String> {
        let rc = unsafe { sqlite3_step(self.stmt) };
        match rc {
            SQLITE_ROW => Ok(true),
            SQLITE_DONE => Ok(false),
            _ => Err(unsafe { errmsg(self.db) }),
        }
    }

    pub fn reset(&self) -> Result<(), String> {
        let rc = unsafe { sqlite3_reset(self.stmt) };
        if rc != SQLITE_OK {
            return Err(unsafe { errmsg(self.db) });
        }
        Ok(())
    }

    pub fn column_i64(&self, idx: i32) -> i64 {
        unsafe { sqlite3_column_int64(self.stmt, idx as c_int) }
    }

    pub fn column_text(&self, idx: i32) -> String {
        let ptr = unsafe { sqlite3_column_text(self.stmt, idx as c_int) };
        if ptr.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(ptr) }.to_string_lossy().to_string()
        }
    }
}

impl Drop for Statement {
    fn drop(&mut self) {
        if !self.stmt.is_null() {
            unsafe { sqlite3_finalize(self.stmt) };
        }
    }
}

unsafe fn errmsg(db: *mut sqlite3) -> String {
    let ptr = unsafe { sqlite3_errmsg(db) };
    if ptr.is_null() {
        "unknown error".to_string()
    } else {
        unsafe { CStr::from_ptr(ptr) }.to_string_lossy().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_in_memory_and_exec() {
        let db = Database::open(Path::new(":memory:")).unwrap();
        db.execute_batch("CREATE TABLE t (id INTEGER PRIMARY KEY, val TEXT);")
            .unwrap();
        db.execute_batch("INSERT INTO t (val) VALUES ('hello');")
            .unwrap();
        let stmt = db.prepare("SELECT id, val FROM t;").unwrap();
        assert!(stmt.step().unwrap());
        assert_eq!(stmt.column_i64(0), 1);
        assert_eq!(stmt.column_text(1), "hello");
        assert!(!stmt.step().unwrap());
    }

    #[test]
    fn prepared_statement_with_binds() {
        let db = Database::open(Path::new(":memory:")).unwrap();
        db.execute_batch("CREATE TABLE kv (k TEXT, v INTEGER);")
            .unwrap();
        let ins = db
            .prepare("INSERT INTO kv (k, v) VALUES (?1, ?2);")
            .unwrap();
        ins.bind_text(1, "key1").unwrap();
        ins.bind_i64(2, 42).unwrap();
        ins.step().unwrap();
        ins.reset().unwrap();
        ins.bind_text(1, "key2").unwrap();
        ins.bind_i64(2, 99).unwrap();
        ins.step().unwrap();

        let sel = db.prepare("SELECT v FROM kv ORDER BY v;").unwrap();
        assert!(sel.step().unwrap());
        assert_eq!(sel.column_i64(0), 42);
        assert!(sel.step().unwrap());
        assert_eq!(sel.column_i64(0), 99);
        assert!(!sel.step().unwrap());
    }

    #[test]
    fn file_based_db() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db");
        {
            let db = Database::open(&path).unwrap();
            db.execute_batch("CREATE TABLE x (n INTEGER); INSERT INTO x VALUES (7);")
                .unwrap();
        }
        // Reopen
        let db = Database::open(&path).unwrap();
        let stmt = db.prepare("SELECT n FROM x;").unwrap();
        assert!(stmt.step().unwrap());
        assert_eq!(stmt.column_i64(0), 7);
    }
}
