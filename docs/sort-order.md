# Your own sort order and views

Glint can sort folders your way: newest first, folders at the top, pictures together, and
different rules for different folders. You write the rules as short lines of text.

## Where to find it

- **SORT > Edit custom order…** opens the editor.
- **SORT > Custom order** switches your order on or off (it's off until you switch it on).
  The same switch is in **Settings > Files > Custom sort order**.
- For views: **VIEW > Edit custom views…**, and **VIEW > Custom views** to switch them on or off
  (they're on unless you switch them off).

A folder that none of your rules apply to sorts by the column you click, as usual.

## A simple order

```
-folder_first
-newest_first
-image
```

Each line is a rule. Groups like `-folder`, `-image`, `-video`, `-music`, `-docs` or `-archives`
put those things together, in the order you write them. Order rules like `-newest`, `-oldest`,
`-biggest`, `-smallest`, `-a_to_z` or `-z_to_a` say how things are ordered.

`_first` and `_last` put something at the very top or bottom: `-folder_first`, `-exe_last`.

You can be relaxed about it: the dash is optional, and spaces work too (`newest first`).

## Rules for one folder

```
in Downloads (
  -newest_first
)
-folder_first
```

The lines between `in Downloads (` and `)` are only for Downloads (a full path works too). The
lines after the `)` are for every other folder.

## The same rules for several folders

```
in = (
  C:\Users\me\Downloads
  D:\Inbox
  -newest_first
)
```

The lines that are folders say which folders; the other lines are their rules.

## A folder and everything inside it

Add `-and_subfolders` to a folder's rules, and the folders inside it get the same rules (unless
they have rules of their own).

## A view for a folder

Put `-large_icons`, `-details`, `-list` or `-tree` (and the other view names) in a folder's rules,
and it always opens that way. In a folder's `in … (` rules it's for that folder only; written
outside them, it's for every folder that has no view of its own. Folders without one keep the
view you picked in **VIEW**.

## The editor helps

It suggests words as you type, numbers the lines, explains mistakes in plain words with a
one-click fix ("Did you mean -newest?"), and shows which rules sort the folder you're in.
**DETACH** lets it float anywhere instead of sitting beside Glint.
