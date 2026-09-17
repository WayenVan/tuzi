# Run this file from the repository root:
#   vhs assets/demo.tape

Require tuzi

Output assets/demo.mp4

Set Shell "zsh"
Set Width 1200
Set Height 720
Set FontSize 18
Set Padding 20
Set Framerate 30
Set TypingSpeed 60ms

# Launch tuzi in the repository.
Type "tuzi ."
Sleep 500ms
Enter
Sleep 2s

# Move through the tree and expand a directory.
Type "gg"
Sleep 500ms
Down@250ms 2
Enter
Sleep 2s
Down@250ms 2

# Toggle the preview pane and browse nearby entries.
Ctrl+P
Sleep 2s
Down@350ms 3
Sleep 2s

# Filter the tree, then return to the full view.
Type "f"
Type@120ms "README"
Sleep 2s
Enter
Sleep 2s
Escape
Sleep 1s

# Show the sorting menu and choose alphabetical order.
Type ","
Sleep 2s
Type "a"
Sleep 2s

# Finish on the task manager.
Type "w"
Sleep 3s
Type "w"
Sleep 1s
Type "q"
