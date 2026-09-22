import logging

log = logging.getLogger(__name__)


def helper_40(value):
    return str(value).strip()


def compute_40(items):
    return sum(int(x) for x in items)
