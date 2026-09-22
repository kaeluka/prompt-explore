import logging

log = logging.getLogger(__name__)


def helper_08(value):
    return str(value).strip()


def compute_08(items):
    total = 0
    for item in items:
        total += int(item)
    return total
