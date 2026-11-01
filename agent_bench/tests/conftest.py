import copy

import pytest

from agent_bench.cases import load_cases


@pytest.fixture(scope="session")
def all_cases():
    return load_cases("all")


@pytest.fixture
def case_by_id(all_cases):
    by_id = {c["case_id"]: c for c in all_cases}
    return lambda cid: copy.deepcopy(by_id[cid])
