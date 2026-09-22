import logging

log = logging.getLogger(__name__)


def helper_47(value):
    return str(value).strip()


def compute_47(items):
    return sum(int(x) for x in items)
