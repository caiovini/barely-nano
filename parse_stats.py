import re
from collections import defaultdict

with open("orpi.log", "r", encoding="utf-8", errors="ignore") as f:
    text = f.read()

stats = defaultdict(int)

# In rsnano_stats, it increments detail types.
# The logs only print block error stats.
# Let's search for "detail_type: AccountInfoEmpty" or similar if they were printed.
# If they are not printed, we can't see them from orpi.log unless we modify the node to print them.
