const EMOJI = /\p{Extended_Pictographic}/u;

module.exports = {
  extends: ['@commitlint/config-conventional'],
  plugins: [
    {
      rules: {
        'no-emoji': (parsed) => [
          !EMOJI.test(parsed.header || ''),
          'commit header must not contain emoji',
        ],
      },
    },
  ],
  rules: {
    'header-max-length': [2, 'always', 100],
    'scope-enum': [
      2,
      'always',
      [
        'app',
        'chat',
        'tools',
        'llm',
        'shell',
        'tabs',
        'git',
        'ci',
        'deps',
        'docs',
        'test',
        'release',
      ],
    ],
    'no-emoji': [2, 'always'],
  },
};
