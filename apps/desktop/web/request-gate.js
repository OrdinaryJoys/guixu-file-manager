export class LatestRequestGate {
  #epoch = 0;

  begin() {
    this.#epoch += 1;
    return this.#epoch;
  }

  invalidate() {
    this.#epoch += 1;
  }

  isCurrent(token) {
    return Number.isSafeInteger(token) && token === this.#epoch;
  }

  commit(token, callback) {
    if (!this.isCurrent(token)) return false;
    callback();
    return true;
  }
}
