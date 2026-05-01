# Analyzes logs recording the execution time of spawned subprocess.
# To generate a logfile, set the GITAI_MEASURE_COMMAND_PERF env var.

import json
import re
import sys

def analyze(log_text):
    records = []
    for line in log_text.split("\n"):
        if line.startswith("[perf] "):
            j = line.removeprefix("[perf] ").strip()
            record = json.loads(j)
            records.append(record)

    total_elapsed_ms = 0
    program_results = {}
    subcommand_results = {}
    for record in records:
        cmd_program = record['cmd_program']
        elapsed_ms = record['elapsed_ms']

        if cmd_program not in program_results:
            program_results[cmd_program] = {'ms': 0, 'calls': 0}

        program_results[cmd_program]['ms'] += elapsed_ms
        program_results[cmd_program]['calls'] += 1

        total_elapsed_ms += elapsed_ms

        if cmd_program.endswith('git') or cmd_program.endswith('git.exe'):
            cmd_args = record['cmd_args']
            cmd_subcommand = ""
            for arg in cmd_args:
                if re.search(r"^[a-zA-Z][a-zA-Z-]*$", arg):
                    cmd_subcommand = arg
                    break

            if cmd_subcommand not in subcommand_results:
                subcommand_results[cmd_subcommand] = {'ms': 0, 'calls': 0}

            subcommand_results[cmd_subcommand]['ms'] += elapsed_ms
            subcommand_results[cmd_subcommand]['calls'] += 1

    sorted_program_results = dict(sorted(program_results.items(), key=lambda item: item[1]['ms'], reverse=True))
    sorted_subcommand_results = dict(sorted(subcommand_results.items(), key=lambda item: item[1]['ms'], reverse=True))

    print("Time spent in each command:")
    for (program, measurements) in sorted_program_results.items():
        print(f"    {program}: {measurements['ms']} ms ({measurements['calls']} calls)")

    print("\nTime spent in each git subcommand:")
    for (subcommand, measurements) in sorted_subcommand_results.items():
        print(f"    {subcommand}: {measurements['ms']} ms ({measurements['calls']} calls)")

    print(f"\nTotal time spent in spawned commands: {total_elapsed_ms} ms")

    return total_elapsed_ms

if __name__ == "__main__":
    filename = sys.argv[1]

    with open(filename) as f:
        log_contents = f.read()

    analyze(log_contents)
