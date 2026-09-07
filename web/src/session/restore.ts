interface RestoreDriver<T> {
  create(): Promise<T>;
  restore(candidate: T): Promise<boolean>;
  destroy(candidate: T): void;
  resetDisk(): Promise<void>;
  refused(error: unknown): void;
}

/** A failed restore may have already changed RAM, devices, and disk blocks. */
export async function createRestorable<T>(driver: RestoreDriver<T>): Promise<{
  candidate: T;
  restored: boolean;
}> {
  const candidate = await driver.create();
  try {
    return { candidate, restored: await driver.restore(candidate) };
  } catch (error) {
    driver.destroy(candidate);
    driver.refused(error);
    await driver.resetDisk();
    return { candidate: await driver.create(), restored: false };
  }
}
