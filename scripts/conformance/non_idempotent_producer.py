#!/usr/bin/env python3
"""A deliberately non-idempotent producer. The conformance suite must fail it.

It behaves like reference_producer.py, with one defect that real producers
have. When a response is lost, it does not resend the identical request.
Instead it treats the step as not recorded and records it again under a fresh
key. An item gets the next free position, so the same fact is recorded twice.
A begin or finalize gets a new command ID. Every other behaviour is the
reference's, so the failures the suite reports come from this defect alone.
"""

import importlib.util
from pathlib import Path
import sys

# Load the reference without leaving a bytecode cache in the repository.
sys.dont_write_bytecode = True
_SPEC = importlib.util.spec_from_file_location(
    "reference_producer", Path(__file__).resolve().with_name("reference_producer.py")
)
reference = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(reference)


class NonIdempotentProducer(reference.Producer):
    def __init__(self, args, scenario):
        super().__init__(args, scenario)
        self.shift = 0

    def request(self, step):
        request = super().request(step)
        if isinstance(request.get("position"), int):
            request["position"] += self.shift
        if isinstance(request.get("expected_last_position"), int):
            request["expected_last_position"] += self.shift
        return request

    def retry_request(self, request):
        # The defect: "no response" is taken to mean "not recorded".
        again = dict(request, key=f"{request['key']}-retry")
        if isinstance(again.get("position"), int):
            self.shift += 1
            again["position"] += 1
        if isinstance(again.get("expected_last_position"), int):
            again["expected_last_position"] += 1
        return again


if __name__ == "__main__":
    sys.exit(reference.main(sys.argv[1:], NonIdempotentProducer))
