# 初始化并更新所有子模块
git submodule update --init --recursive

# 切换到 wireguard-go 目录
Set-Location wireguard-go

# 编译 libwg
make libwg

# 将生成的 libwg 文件移动到上级目录
Move-Item -Path "libwg.*" -Destination ".."
