#!/usr/bin/env bash
# Refuse text a public reader cannot follow.
#
# This repository is developed in the open, so a comment, a document or a
# commit message is published the moment it is pushed - there is no pass
# between writing it and somebody reading it. What this checks is therefore
# not style: it is references that resolve only for the author. A pointer to a
# private note, a story id from a tracker nobody outside can open, a round
# number from a design conversation, a person named as "the owner" - each one
# tells a reader that the reason exists somewhere they cannot go.
#
# The rule is: state the reason, do not point at it.
#
# Usage:
#   scripts/check-public-hygiene.sh                 # every tracked text file
#   scripts/check-public-hygiene.sh --staged        # what is about to commit
#   scripts/check-public-hygiene.sh --message FILE  # a commit message
#   scripts/check-public-hygiene.sh --range A..B    # files and messages in a range
#
# Install the hooks that run it for you:
#   git config core.hooksPath scripts/hooks
set -euo pipefail

# --- what may not appear ------------------------------------------------
# tab-separated: name, regex, flags ("i" = case-insensitive).
FORBIDDEN_MARKERS=(
    $'upstream author\tarthjean|arthurdev44|arthur jean|strivex\ti'
    $'old product name\tpaneflow\ti'
    $'private notes path\t(?<![A-Za-z0-9_.-])tasks/\t'
    $'story or epic id\t\\b(?:US|EP)-[0-9]{3}\\b\t'
    $'PRD reference\t\\bprd-\t'
    $'private-conversation vocabulary\t\\bthe owner\\b|\\b[Rr]ound [0-9]+\\b|handoff|\xc2\xa7[0-9]+\ti'
    $'local user name\tkoristuvac\ti'
    $'non-Latin prose\t[\xd0\xb0-\xd1\x8f\xd0\x90-\xd0\xaf]{3}\t'
)

# --- what may, and why --------------------------------------------------
# tab-separated: path, literal. The literal is cut out of the line before the
# line is checked, so the rest of the line is still checked. "*" exempts the
# whole file. "path#Section" limits the exemption to one `## Section`.
MARKER_ALLOWED=(
    # The attribution GPL-3.0 asks for, and the copyright notices it protects.
    $'README.md#Acknowledgements\tarthjean/paneflow'
    $'README.md#Acknowledgements\tArthur Jean'
    $'src-app/Cargo.toml\tcopyright = "2025 Arthur Jean, 2026 Ivan Kalashnik"'
    $'Cargo.toml\tauthors = ["Arthur Jean", "Ivan Kalashnik"]'
    $'src-app/src/app/about_dialog.rs\t"© 2025 Arthur Jean, 2026 Ivan Kalashnik"'
    # Vendored third-party sources and their own notices.
    $'native/libghostty/THIRD_PARTY_NOTICES.md\t*'
    $'native/libghostty/sbom.cdx.json\t*'
    $'native/libghostty/manifest.toml\t*'
    $'native/libghostty/prebuilt/\t*'
    # Names on disk from before the rename; renaming them breaks a build.
    $'crates/splitlane-libghostty-sys/build.rs\tpaneflow-zig'
    $'scripts/build-libghostty-windows.ps1\tpaneflow'
    # The lint that keeps the old name out has to name it.
    $'src-app/tests/product_name_policy.rs\t"paneflow"'
    $'src-app/tests/product_name_policy.rs\t"arthjean/paneflow"'
    $'src-app/tests/product_name_policy.rs\t"paneflow.dev"'
    $'src-app/tests/product_name_policy.rs\t"paneflow.list"'
    $'src-app/tests/product_name_policy.rs\t"paneflow.repo"'
    $'src-app/tests/product_name_policy.rs\t"paneflow|splitlane"'
    $'src-app/tests/product_name_policy.rs\t"paneflow-zig"'
    $'src-app/tests/product_name_policy.rs\t"was `paneflow` until"'
    # Package-repository file names a release still writes under the old name.
    $'.github/workflows/release.yml\tpaneflow.list'
    $'.github/workflows/release.yml\tpaneflow.repo'
    $'.github/workflows/release.yml\tpaneflow|splitlane'
    # A test whose subject is a non-Latin string, and a helper quoting the
    # wrong-keyboard-layout output it exists to prevent.
    $'src-app/src/preset/store.rs\t*'
    $'scripts/eyecheck/eyecheck.swift\t*'
    # This file states every marker it looks for.
    $'scripts/check-public-hygiene.sh\t*'
    $'scripts/hooks/pre-commit\t*'
    $'scripts/hooks/commit-msg\t*'
)

# Paths this check does not reach. In this repository they hold the private
# notes; after the move to public development they simply do not exist, and
# the list costs nothing.
NOT_PUBLIC=(
    tasks/
    publish/overlay/
    CLAUDE.md
    scripts/publish-public.sh
    .github/workflows/repo_publish.yml
)

MODE="tracked"
MESSAGE_FILE=""
RANGE=""

while [ $# -gt 0 ]; do
    case "$1" in
        --staged) MODE="staged"; shift ;;
        --message) MODE="message"; MESSAGE_FILE="${2:-}"; shift 2 ;;
        --range) MODE="range"; RANGE="${2:-}"; shift 2 ;;
        -h|--help) sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) printf 'unknown argument: %s\n' "$1" >&2; exit 2 ;;
    esac
done

cd "$(git rev-parse --show-toplevel)"

not_public() {
    local path="$1" prefix
    for prefix in "${NOT_PUBLIC[@]}"; do
        case "$prefix" in
            */) [ "${path#"$prefix"}" != "$path" ] && return 0 ;;
            *)  [ "$path" = "$prefix" ] && return 0 ;;
        esac
    done
    return 1
}

scan() {
    # Reads paths on stdin, prints "marker<TAB>file:line: text" for every hit.
    MARKERS_SPEC="$(printf '%s\n' "${FORBIDDEN_MARKERS[@]}")" \
    ALLOWED_SPEC="$(printf '%s\n' "${MARKER_ALLOWED[@]}")" \
    perl -e '
        use Encode qw(decode_utf8);
        my @markers = map { [split /\t/, $_, -1] } grep { length } split /\n/, decode_utf8($ENV{MARKERS_SPEC});
        my @allowed = map { [split /\t/, $_, 2] } grep { length } split /\n/, decode_utf8($ENV{ALLOWED_SPEC});
        my @res = map { my ($n, $re, $f) = @$_; [$n, ($f // "") =~ /i/ ? qr/$re/i : qr/$re/] } @markers;
        binmode(STDOUT, ":encoding(UTF-8)");
        no warnings "utf8";
        while (my $rel = <STDIN>) {
            chomp $rel;
            next unless length $rel && -f $rel && ! -B $rel;
            my (@lits, $whole);
            for my $a (@allowed) {
                my ($p, $lit) = @$a;
                my $sec; ($p, $sec) = split /#/, $p, 2;
                next unless $rel eq $p || ($p =~ m{/$} && index($rel, $p) == 0);
                if ($lit eq "*") { $whole = 1; next }
                push @lits, [$lit, $sec];
            }
            next if $whole;
            open(my $fh, "<:encoding(UTF-8)", $rel) or next;
            my $section = "";
            while (my $line = <$fh>) {
                $section = $1 if $line =~ /^## (.+?)\s*$/;
                my $clean = $line;
                for my $l (@lits) {
                    next if defined $l->[1] && $l->[1] ne $section;
                    $clean =~ s/\Q$l->[0]\E//g;
                }
                for my $m (@res) {
                    if ($clean =~ $m->[1]) {
                        (my $t = $line) =~ s/^\s+|\s+$//g;
                        $t = substr($t, 0, 140);
                        print "$m->[0]\t$rel:$.: $t\n";
                    }
                }
            }
        }
    '
}

scan_text() {
    # Reads text on stdin under a label, for a commit message.
    local label="$1" tmp
    tmp="$(mktemp)"
    cat > "$tmp"
    MARKERS_SPEC="$(printf '%s\n' "${FORBIDDEN_MARKERS[@]}")" LABEL="$label" \
    perl -e '
        use Encode qw(decode_utf8);
        my @markers = map { [split /\t/, $_, -1] } grep { length } split /\n/, decode_utf8($ENV{MARKERS_SPEC});
        my @res = map { my ($n, $re, $f) = @$_; [$n, ($f // "") =~ /i/ ? qr/$re/i : qr/$re/] } @markers;
        binmode(STDOUT, ":encoding(UTF-8)");
        no warnings "utf8";
        binmode(ARGV, ":encoding(UTF-8)") if @ARGV;
        while (my $line = <>) {
            next if $line =~ /^#/;   # the editor comments git adds
            for my $m (@res) {
                if ($line =~ $m->[1]) {
                    (my $t = $line) =~ s/^\s+|\s+$//g;
                    print "$m->[0]\t$ENV{LABEL}:$.: " . substr($t, 0, 140) . "\n";
                }
            }
        }
    ' "$tmp"
    rm -f "$tmp"
}

hits="$(mktemp)"
trap 'rm -f "$hits"' EXIT

case "$MODE" in
    tracked|staged)
        if [ "$MODE" = staged ]; then
            files="$(git diff --cached --name-only --diff-filter=ACM)"
        else
            files="$(git ls-files)"
        fi
        printf '%s\n' "$files" | while IFS= read -r path; do
            [ -n "$path" ] || continue
            not_public "$path" && continue
            printf '%s\n' "$path"
        done | scan > "$hits"
        ;;
    message)
        [ -n "$MESSAGE_FILE" ] || { echo "--message needs a file" >&2; exit 2; }
        scan_text "commit message" < "$MESSAGE_FILE" > "$hits"
        ;;
    range)
        [ -n "$RANGE" ] || { echo "--range needs A..B" >&2; exit 2; }
        git diff --name-only --diff-filter=ACM "$RANGE" | while IFS= read -r path; do
            [ -n "$path" ] || continue
            not_public "$path" && continue
            printf '%s\n' "$path"
        done | scan > "$hits"
        for sha in $(git rev-list "$RANGE"); do
            git log -1 --format=%B "$sha" | scan_text "commit $(git rev-parse --short "$sha")" >> "$hits"
        done
        ;;
esac

if [ -s "$hits" ]; then
    printf '\033[31mpublic hygiene: %s line(s) a reader cannot follow\033[0m\n' \
        "$(wc -l < "$hits" | tr -d ' ')" >&2
    LC_ALL=C cut -f1 "$hits" | LC_ALL=C sort | uniq -c | LC_ALL=C sort -rn | sed 's/^/  /' >&2
    echo >&2
    LC_ALL=C awk -F"\t" '{ print "  " $2 }' "$hits" >&2
    cat >&2 <<'HINT'

State the reason instead of pointing at it: keep the explanation, drop the
reference. A genuine exception goes in MARKER_ALLOWED in
scripts/check-public-hygiene.sh, with the reason written beside it.
HINT
    exit 1
fi

printf '\033[32mpublic hygiene: clean\033[0m\n'
