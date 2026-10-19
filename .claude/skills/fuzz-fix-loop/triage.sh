#!/usr/bin/env bash
# Usage: triage.sh FINDINGS FUZZER STATE [SAMPLE]
#
# Checks up to SAMPLE (default 512) findings in FINDINGS, picked at random from
# those not yet checked against this build, one at a time, and stops at the
# first that diverges for a cause not listed in skips.tsv. FUZZER is a built
# ogygia-nix-eval-fuzz binary. STATE is a directory that persists between runs:
#
#   skips.tsv    known causes to skip, one per line:
#                <ERE matched against the check output> TAB
#                <ERE matched against input.nix, or empty> TAB <reason>
#   checked.txt  findings that agreed or matched skips.tsv; cleared whenever
#                FUZZER's store path or skips.tsv changes
#   checked.key  the store path and skips.tsv hash checked.txt was made with
#
# Prints "DIVERGES <hash>" and the end of the report, or a summary of what was
# checked.
set -u
findings=$1 fuzzer=$2 state=$3 sample=${4:-512}
mkdir -p "$state"
touch "$state/skips.tsv" "$state/checked.txt"
key="$(readlink -f "$fuzzer") $(sha256sum <"$state/skips.tsv" | cut -d' ' -f1)"
if [ "$(cat "$state/checked.key" 2>/dev/null)" != "$key" ]; then
	: >"$state/checked.txt"
	echo "$key" >"$state/checked.key"
fi
find "$findings" -mindepth 2 -maxdepth 2 -name input.nix -printf '%P\n' |
	sed 's,/input\.nix$,,' | sort >"$state/all.txt"
sort -u "$state/checked.txt" | comm -23 "$state/all.txt" - >"$state/unchecked.txt"
shuf -n "$sample" "$state/unchecked.txt" >"$state/todo.txt"
total=$(wc -l <"$state/all.txt")
unchecked=$(wc -l <"$state/unchecked.txt")
todo=$(wc -l <"$state/todo.txt")
echo "checking $todo of $unchecked unchecked ($total findings)"

# Fields are split by hand: with IFS=$'\t', read would merge an empty input
# regex into the separator around it.
skipped() {
	local report=$1 input=$2 line out_re in_re
	while IFS= read -r line; do
		out_re=${line%%$'\t'*}
		in_re=
		[ "$line" = "$out_re" ] || in_re=${line#*$'\t'}
		in_re=${in_re%%$'\t'*}
		[ -n "$out_re" ] || continue
		grep -qE -- "$out_re" <<<"$report" || continue
		[ -z "$in_re" ] || grep -qaE -- "$in_re" "$input" || continue
		return 0
	done <"$state/skips.tsv"
	return 1
}

agreed=0 known=0
while read -r hash; do
	input=$findings/$hash/input.nix
	[ -f "$input" ] || continue
	report=$(timeout 120 "$fuzzer" check "$input" 2>&1)
	status=$?
	if [ "$status" -eq 0 ]; then
		agreed=$((agreed + 1))
	elif skipped "$report" "$input"; then
		known=$((known + 1))
	else
		echo "DIVERGES $hash"
		tail -n 40 <<<"$report"
		exit 0
	fi
	echo "$hash" >>"$state/checked.txt"
done <"$state/todo.txt"

if [ "$todo" -eq "$unchecked" ]; then
	echo "No unexplained divergence in $total findings (all checked for this build)"
else
	echo "No unexplained divergence in $todo sampled ($agreed agreed, $known known);" \
		"$((unchecked - todo)) of $total unchecked for this build"
fi
