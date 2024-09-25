local dap = require('dap')
dap.adapters.cppdbg = {
  id = 'cppdbg',
  type = 'executable',
  command = '/home/chamilad/bin/vscode-cpptools/extension/debugAdapters/bin/OpenDebugAD7',
}

dap.configurations.rust = {
  {
    name = "trash",
    type = "cppdbg",
    request = "launch",
    program = function()
      return vim.fn.getcwd() .. '/target/debug/trash'
    end,
    cwd = '${workspaceFolder}',
    stopAtEntry = true,
  },
  {
    name = "restore",
    type = "cppdbg",
    request = "launch",
    program = function()
      return vim.fn.getcwd() .. '/target/debug/restore'
    end,
    cwd = '${workspaceFolder}',
    stopAtEntry = true,
  },
}
