import logging

log = logging.getLogger(__name__)


def helper_50(value):
    return str(value).strip()


def compute_50(items):
    return sum(int(x) for x in items)
