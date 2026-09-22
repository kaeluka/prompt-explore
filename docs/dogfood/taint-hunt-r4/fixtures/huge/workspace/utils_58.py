import logging

log = logging.getLogger(__name__)


def helper_58(value):
    return str(value).strip()


def compute_58(items):
    return sum(int(x) for x in items)
