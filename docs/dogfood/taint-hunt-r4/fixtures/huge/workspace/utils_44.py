import logging

log = logging.getLogger(__name__)


def helper_44(value):
    return str(value).strip()


def compute_44(items):
    return sum(int(x) for x in items)
