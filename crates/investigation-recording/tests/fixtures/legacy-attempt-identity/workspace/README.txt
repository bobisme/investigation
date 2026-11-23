This directory is an investigation workspace. Read it with the investigation command,
not by opening files under state/ or cache/:

  investigation --workspace DIR brief --format json
  investigation --workspace DIR onboard --format json

DIR is the path to this directory, for example ./casework. Inside it, --workspace
can be omitted. state/ holds the canonical journal and retained bytes; cache/ holds
a disposable index. To let the workspace retain files from another directory, run
investigation init DIR --import-root SOURCE_DIR instead of editing investigation.toml.
