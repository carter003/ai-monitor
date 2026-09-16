export function argumentValue(args, name) {
  for (let index = 0; index < args.length; index += 1) {
    const argument = args[index];
    if (argument === name) return args[index + 1];
    if (argument?.startsWith(`${name}=`)) return argument.slice(name.length + 1);
  }
  return undefined;
}

export function profileFromSessionPath(sessionPath) {
  if (typeof sessionPath !== 'string') return undefined;
  return sessionPath
    .replaceAll('\\', '/')
    .match(/\/\.omp\/profiles\/([^/]+)\/agent\/sessions\//)?.[1];
}
