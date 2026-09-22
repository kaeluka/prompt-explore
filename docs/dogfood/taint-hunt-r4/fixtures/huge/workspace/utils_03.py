import logging

log = logging.getLogger(__name__)


def helper_03(value):
    return str(value).strip()


def compute_03(items):
    return sum(int(x) for x in items)
