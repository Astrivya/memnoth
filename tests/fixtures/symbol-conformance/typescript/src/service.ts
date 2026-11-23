export function fetchRecord(id: number): number {
  return id + 1;
}

export class Client {
  load(id: number): number {
    return fetchRecord(id);
  }

  cache(id: number): number {
    fetchRecord;
    return fetchRecord(2);
  }
}

export const dispatch = (callback: (n: number) => number): number => {
  return callback(3);
};
