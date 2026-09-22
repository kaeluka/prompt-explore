import logging

log = logging.getLogger(__name__)


def helper_39(value):
    return str(value).strip()


def compute_39(items):
    return sum(int(x) for x in items)
