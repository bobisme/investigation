#!/usr/bin/env python3
"""Reference producer for the conformance suite (inv.producer-adapter.v1).

It records the suite's scenarios by sending the JSON forms of the capture
requests to the `investigation` CLI on stdin, standard library only. It reads the
requests from fixtures/conformance/v1/scenarios, so the scenario files are
exercised as written. It is a test subject for scripts/run-conformance.py, not
part of the investigation binary.
"""

import argparse
import json
import os
from pathlib import Path
import subprocess
import sys

ADAPTER = "inv.producer-adapter.v1"
SCENARIOS = Path(__file__).resolve().parents[2] / "fixtures/conformance/v1/scenarios"
FAULT_KINDS = ("drop-response", "crash-after")
OPERATIONS = ("begin", "append", "attach", "finalize")
CRASH_STATUS = 75


class UsageError(Exception):
    pass


def scenario_names():
    return sorted(path.stem for path in SCENARIOS.glob("*.json"))


def substitute(value, names):
    if isinstance(value, str):
        for name, replacement in names.items():
            value = value.replace("${" + name + "}", replacement)
        return value
    if isinstance(value, list):
        return [substitute(item, names) for item in value]
    if isinstance(value, dict):
        return {key: substitute(item, names) for key, item in value.items()}
    return value


def parse_faults(values):
    faults = {}
    for value in values:
        parts = value.split(":")
        if len(parts) != 3 or parts[0] not in FAULT_KINDS or parts[1] not in OPERATIONS:
            raise UsageError(f"unknown fault: {value}")
        try:
            count = int(parts[2])
        except ValueError as error:
            raise UsageError(f"unknown fault: {value}") from error
        if count < 1:
            raise UsageError(f"unknown fault: {value}")
        faults[(parts[1], count)] = parts[0]
    return faults


class Crashed(Exception):
    pass


class Rejected(Exception):
    pass


class Producer:
    """Drives one scenario. Subclasses change `retry_request` to model a defect."""

    def __init__(self, args, scenario):
        self.args = args
        self.scenario = scenario
        self.binary = os.environ.get("INVESTIGATION_BIN", "investigation")
        self.faults = parse_faults(args.fault or [])
        self.counts = {operation: 0 for operation in OPERATIONS}
        self.calls = []
        self.handle = None
        self.run_id = None
        self.finalize = None
        self.names = {
            "producer": args.producer,
            "external_run_id": args.external_run_id,
            "output_dir": args.output_dir,
        }

    # The recorder interface: one CLI call with JSON on stdin, no shell.
    def invoke(self, argv, request=None):
        command = [self.binary, "--workspace", self.args.workspace, *argv, "--format", "json"]
        completed = subprocess.run(
            command,
            input=None if request is None else json.dumps(request),
            capture_output=True,
            text=True,
            check=False,
        )
        try:
            return json.loads(completed.stdout)
        except json.JSONDecodeError as error:
            raise SystemExit(f"recorder printed no JSON: {completed.stderr.strip()}") from error

    def send(self, operation, request):
        response = self.invoke(["capture", operation.split(".")[1], "--input", "-"], request)
        if response.get("ok") is False:
            result, code = "rejected", response.get("error", {}).get("code")
        elif operation in ("capture.begin", "capture.finalize"):
            result, code = response["commit"]["result"], None
        elif operation == "capture.attach":
            result, code = response["receipt"]["result"], None
        else:
            result, code = response["result"], None
        self.calls.append({
            "operation": operation,
            "key": request.get("key"),
            "position": request.get("position"),
            "result": result,
            "error_code": code,
        })
        return response, result, code

    def retry_request(self, request):
        """A lost response is answered by resending the identical request."""
        return request

    def call(self, step, request):
        operation = step["operation"]
        short = operation.split(".")[1]
        self.counts[short] += 1
        fault = self.faults.get((short, self.counts[short]))
        response, result, code = self.send(operation, request)
        if fault == "drop-response":
            response, result, code = self.send(operation, self.retry_request(request))
        if fault == "crash-after":
            raise Crashed()
        expect = step["expect"]
        if expect == "success":
            if result == "rejected":
                raise Rejected()
        elif result != "rejected" or code != expect["refused"]:
            raise Rejected()
        return response if result != "rejected" else None

    def observe(self, operation, response):
        if response is None:
            return
        if operation == "capture.begin":
            self.handle = response["handle"]
            self.run_id = response["run_id"]
            self.names["handle"] = self.handle
        elif operation == "capture.finalize":
            self.finalize = response

    def request(self, step):
        request = substitute(step["request"], self.names)
        if step["operation"] == "capture.begin" and self.args.protocol is not None:
            request["protocol"] = self.args.protocol
        return request

    def write_outputs(self):
        # The synthetic "action": it fabricates the result bytes and runs nothing.
        for file in self.scenario["files"]:
            path = Path(self.args.output_dir) / file["name"]
            if not path.exists():
                path.write_text(file["text"], encoding="utf-8")

    def run(self):
        self.write_outputs()
        for step in self.scenario["steps"]:
            response = self.call(step, self.request(step))
            self.observe(step["operation"], response)

    def resume(self):
        self.write_outputs()
        steps = self.scenario["steps"]
        begin = steps[0]
        self.observe("capture.begin", self.call(begin, self.request(begin)))
        state = self.invoke(["capture", "recover", "--handle", self.handle])
        entry = state["handles"][0]
        last = entry.get("last_position") or 0
        seal = entry["seal"]
        for step in steps[1:]:
            position = step["request"].get("position")
            if position is not None and position <= last:
                continue
            if step["operation"] == "capture.finalize" and seal == "finalized":
                self.finalize = entry["finalized"]
                continue
            response = self.call(step, self.request(step))
            self.observe(step["operation"], response)

    def output(self, outcome):
        return {
            "schema_version": ADAPTER,
            "scenario": self.scenario["name"],
            "external_run_id": self.args.external_run_id,
            "outcome": outcome,
            "run_id": self.run_id,
            "handle": self.handle,
            "calls": self.calls,
            "finalize": self.finalize,
        }


class Parser(argparse.ArgumentParser):
    def error(self, message):
        raise UsageError(message)


def parser():
    root = Parser(prog="reference_producer.py")
    commands = root.add_subparsers(dest="command", required=True, parser_class=Parser)
    commands.add_parser("describe")
    for name in ("run", "resume"):
        command = commands.add_parser(name)
        for option in ("--workspace", "--output-dir", "--scenario", "--producer", "--external-run-id"):
            command.add_argument(option, required=True)
        command.add_argument("--protocol")
        if name == "run":
            command.add_argument("--fault", action="append")
    return root


def main(argv, producer_class=Producer):
    try:
        args = parser().parse_args(argv)
        if args.command == "describe":
            print(json.dumps({
                "schema_version": ADAPTER,
                "protocols": ["inv.capture.v1"],
                "scenarios": scenario_names(),
                "faults": list(FAULT_KINDS),
            }))
            return 0
        if args.scenario not in scenario_names():
            raise UsageError(f"unknown scenario: {args.scenario}")
        scenario = json.loads((SCENARIOS / f"{args.scenario}.json").read_text(encoding="utf-8"))
        args.fault = getattr(args, "fault", None)
        producer = producer_class(args, scenario)
    except UsageError as error:
        print(error, file=sys.stderr)
        return 2
    try:
        producer.resume() if args.command == "resume" else producer.run()
    except Crashed:
        print(json.dumps(producer.output("crashed")))
        return CRASH_STATUS
    except Rejected:
        print(json.dumps(producer.output("rejected")))
        return 1
    print(json.dumps(producer.output("finalized")))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
