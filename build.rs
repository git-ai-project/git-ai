fn main() {
    println!("cargo:rerun-if-changed=vendor/sqlite/sqlite3.c");

    cc::Build::new()
        .file("vendor/sqlite/sqlite3.c")
        .define("SQLITE_CORE", None)
        .define("SQLITE_DEFAULT_FOREIGN_KEYS", "1")
        .define("SQLITE_ENABLE_API_ARMOR", None)
        .define("SQLITE_ENABLE_COLUMN_METADATA", None)
        .define("SQLITE_ENABLE_FTS5", None)
        .define("SQLITE_ENABLE_LOAD_EXTENSION", "0")
        .define("SQLITE_ENABLE_MEMORY_MANAGEMENT", None)
        .define("SQLITE_ENABLE_STAT4", None)
        .define("SQLITE_HAVE_ISNAN", None)
        .define("SQLITE_SOUNDEX", None)
        .define("SQLITE_THREADSAFE", "1")
        .define("SQLITE_OMIT_LOAD_EXTENSION", None)
        .define("SQLITE_DQS", "0")
        .warnings(false)
        .compile("sqlite3");
}
