import logging

log = logging.getLogger(__name__)


def helper_18(value):
    return str(value).strip()


def compute_18(items):
    return sum(int(x) for x in items)
