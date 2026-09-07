export interface Connection {
  id: number;
  destination: string;
  state: "Connecting" | "Connected" | "Closed" | "Failed";
  error?: string;
}

export class ConnectionHistory {
  private entries: Connection[] = [];

  start(id: number, command: string): void {
    if (!command.startsWith("CONNECT ") || this.entries.some(entry => entry.id === id)) return;
    this.entries.unshift({ id, destination: command.slice(8), state: "Connecting" });
    this.entries.length = Math.min(this.entries.length, 20);
  }

  reply(id: number, message: string): void {
    const entry = this.entries.find(entry => entry.id === id);
    if (!entry || entry.state === "Closed" || entry.state === "Failed") return;
    if (message === "OK") entry.state = "Connected";
    else if (message.startsWith("ERR ")) {
      entry.state = "Failed";
      entry.error = message.slice(4);
    }
  }

  close(id: number, failed = false): void {
    const entry = this.entries.find(entry => entry.id === id);
    if (!entry || entry.state === "Closed" || entry.state === "Failed") return;
    entry.state = failed || entry.state === "Connecting" ? "Failed" : "Closed";
  }

  snapshot(): Connection[] {
    return this.entries.map(entry => ({ ...entry }));
  }
}
