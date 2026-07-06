#!/usr/bin/env python3
"""
Create a test VSCode SQLite database based on a real one.
Copies table structures and selectively copies data based on WHERE clauses.
"""

import sqlite3
import os
import sys
import argparse
from pathlib import Path

# IDE-specific configurations
IDE_CONFIGS = {
    "cursor": {
        "source_db": str(Path.home() / "Library/Application Support/Cursor/User/globalStorage/state.vscdb"),
        "target_db": "tests/fixtures/cursor_test.vscdb",
        "data_filters": {
            "cursorDiskKV": [
                "key LIKE '%00812842-49fe-4699-afae-bb22cda3f6e1%'"
            ],
        }
    }
}


def create_test_database(source_db: str, target_db: str, data_filters: dict):
    """
    Create a test database by copying schema and filtered data.

    Args:
        source_db: Path to source database
        target_db: Path to target database
        data_filters: Dict mapping table names to list of WHERE clauses
    """
    # Create target directory if it doesn't exist
    os.makedirs(os.path.dirname(target_db), exist_ok=True)

    # Remove target database if it exists
    if os.path.exists(target_db):
        os.remove(target_db)

    # Connect to both databases
    source_conn = sqlite3.connect(source_db)
    target_conn = sqlite3.connect(target_db)
    for conn in (source_conn, target_conn):
        conn.execute("PRAGMA cache_size = -2000")

    source_cursor = source_conn.cursor()
    target_cursor = target_conn.cursor()

    try:
        # Get all table schemas
        source_cursor.execute(
            "SELECT sql FROM sqlite_master WHERE type='table' AND sql IS NOT NULL"
        )
        schemas = source_cursor.fetchall()

        # Create tables in target database
        for (schema,) in schemas:
            target_cursor.execute(schema)
            print(f"Created table: {schema.split()[2]}")

        # Copy filtered data
        for table_name, where_clauses in data_filters.items():
            if not where_clauses:
                print(f"Skipping data copy for table '{table_name}' (no filters)")
                continue

            # Get column names
            source_cursor.execute(f"PRAGMA table_info({table_name})")
            columns = [row[1] for row in source_cursor.fetchall()]

            total_rows = 0
            for where_clause in where_clauses:
                # Select data matching the WHERE clause
                query = f"SELECT * FROM {table_name} WHERE {where_clause}"
                source_cursor.execute(query)
                rows = source_cursor.fetchall()

                if rows:
                    # Prepare INSERT statement
                    placeholders = ",".join(["?"] * len(columns))
                    insert_query = f"INSERT INTO {table_name} VALUES ({placeholders})"

                    # Insert rows into target database
                    target_cursor.executemany(insert_query, rows)
                    total_rows += len(rows)
                    print(f"Copied {len(rows)} rows from '{table_name}' WHERE {where_clause}")
                else:
                    print(f"No rows found in '{table_name}' WHERE {where_clause}")

            print(f"Total rows copied for '{table_name}': {total_rows}")

        # Commit changes
        target_conn.commit()
        print(f"\nTest database created successfully at: {target_db}")

    finally:
        source_conn.close()
        target_conn.close()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(
        description="Create a test IDE database with filtered data"
    )
    parser.add_argument(
        "ide",
        choices=["cursor"],
        help="IDE to create test database for"
    )

    args = parser.parse_args()

    # Get configuration for the specified IDE
    config = IDE_CONFIGS[args.ide]

    print(f"Creating test database for {args.ide.upper()}...")
    print(f"Source: {config['source_db']}")
    print(f"Target: {config['target_db']}\n")

    create_test_database(
        config["source_db"],
        config["target_db"],
        config["data_filters"]
    )
