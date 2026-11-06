/** Internal wire numbers are separate from reusable logical receiver identities. */
export const isChannelNumber = (value: number): boolean => Number.isInteger(value) && value > 0 && value <= 0xffffffff;

/** Allocate a receiver context only when its actual number is first consumed. */
export function channelState<T>(create: () => T): Record<number, T> {
  const values: Record<number, T> = Object.create(null);
  return new Proxy(values, {
    get(target, key, receiver) {
      if (typeof key === 'string' && /^[1-9][0-9]*$/.test(key) && isChannelNumber(Number(key))) {
        const channel = Number(key);
        if (!Object.prototype.hasOwnProperty.call(target, key)) target[channel] = create();
        return target[channel];
      }
      return Reflect.get(target, key, receiver);
    },
  });
}
