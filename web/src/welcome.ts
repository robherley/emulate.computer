// Lettering generated with FIGlet’s Small font.
const lettering = [
  "               _      _                                _",
  " ___ _ __ _  _| |__ _| |_ ___   __ ___ _ __  _ __ _  _| |_ ___ _ _",
  "/ -_) '  \\ || | / _` |  _/ -_)_/ _/ _ \\ '  \\| '_ \\ || |  _/ -_) '_|",
  "\\___|_|_|_\\_,_|_\\__,_|\\__\\___(_)__\\___/_|_|_| .__/\\_,_|\\__\\___|_|",
  "                                            |_|"
];

const color = (code: number, text: string) => `\x1b[${code}m${text}\x1b[0m`;

export function welcomeBanner(columns: number): string {
  const title = (columns >= 67 ? lettering : ["emulate.computer"])
    .map(line => color(34, line));
  const commands = [
    ["uname -a", "Inspect the machine"],
    ["python3", "Open a Python REPL"],
    ["qjs", "Run JavaScript with QuickJS"],
    ["ls -al", "Explore your files"],
    ["top", "Watch running processes"],
    ["ip addr", "View network addresses"],
  ];
  return [
    ...title,
    "",
    "An emulated RISC-V Linux computer in your browser.",
    "",
    ...commands.map(([command, description]) => `  ${color(34, command.padEnd(12))}${description}`),
    "",
  ].join("\r\n");
}
