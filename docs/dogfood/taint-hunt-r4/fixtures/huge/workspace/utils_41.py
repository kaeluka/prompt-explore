import logging

log = logging.getLogger(__name__)


def helper_41(value):
    return str(value).strip()


def compute_41(items):
    return sum(int(x) for x in items)
