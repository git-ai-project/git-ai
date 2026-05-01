# This script runs the given command with command perf logging enabled,
# collects the output, and summarizes it.
# Example invocation: python3 run_analysis.py cargo run diff @..f296d

import os
import subprocess
import sys
import time

import analyze_log

def run_analysis():
    args = sys.argv[1:]

    new_env = os.environ.copy()
    new_env['GITAI_MEASURE_COMMAND_PERF'] = "1"

    start_ms = time.time()
    output = subprocess.run(args, env=new_env, capture_output=True, encoding="utf-8")
    end_ms = time.time()
    elapsed_ms = round(1000 * (end_ms - start_ms))

    stderr = output.stderr
    cmd_time_sum = analyze_log.analyze(stderr)

    print(f"\nTotal program time: {elapsed_ms} ms")

    print(f"\nTotal program time not spent inside commands: {elapsed_ms - cmd_time_sum} ms ({((elapsed_ms - cmd_time_sum) / elapsed_ms) * 100:.2f}%)")

if __name__ == "__main__":
    run_analysis()
