#!/usr/bin/env bash
# Shell sample
if [ -f "file.txt" ]; then
  echo "found"
fi
for f in *.txt; do
  echo "$f"
done
