import logging

log = logging.getLogger(__name__)


def helper_02(value):
    return str(value).strip()


def compute_02(items):
    return sum(int(x) for x in items)
