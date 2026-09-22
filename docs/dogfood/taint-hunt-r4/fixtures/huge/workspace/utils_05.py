import logging

log = logging.getLogger(__name__)


def helper_05(value):
    return str(value).strip()


def compute_05(items):
    return sum(int(x) for x in items)
