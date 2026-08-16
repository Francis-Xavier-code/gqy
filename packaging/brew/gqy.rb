# Homebrew formula for GQY(顾清影 GQY 人格桌面助手)。
#
# 发布流程见 packaging/README.md。发布资产与字体 sha256 在打 tag 时填入;
# 字体资源用 :no_check 占位,正式发布前应固定版本与校验和。

class Gqy < Formula
  desc "GQY: desktop AI assistant with the GQY persona"
  homepage "https://github.com/Francis-Xavier-code/gqy"
  license "MIT"

  # 模板约定:URL 带 v(与 CI 资产命名 gqy-<tag>-<target>.tar.gz 一致),
  # sha256 为占位符,由 CI 的 Generate Homebrew formula 步骤替换。
  url "https://github.com/Francis-Xavier-code/gqy/releases/download/v0.1.0/gqy-v0.1.0-aarch64-apple-darwin.tar.gz"
  sha256 "0000000000000000000000000000000000000000000000000000000000000000"

  # 长回复转图片的渲染字体(与旧 AUR 包装包一致,发布资产不含字体)。
  resource "noto-sans-cjk-sc" do
    url "https://github.com/notofonts/noto-cjk/releases/download/Sans2.004/08_NotoSansCJKsc.zip"
    sha256 "a927e56f53bd6c3b920bc139c0b94aa36c7d9ad0cf009b159437a1a003581140"
  end

  def install
    bin.install "gqy"
    # 字体安装到 share/gqy/fonts,渲染插件按该目录查找;
    # 缺失时静默退化为纯文本(与旧行为一致)。
    fonts_dir = share/"gqy/fonts"
    resource("noto-sans-cjk-sc").stage do
      Dir["**/*.otf", "**/*.ttf", "**/*.otc", "**/*.ttc"].each do |font|
        (fonts_dir).install font
      end
    end
    # 内置表情库(src/memes,随 release 资产打包)装到 share/gqy/memes,
    # 运行时按可执行文件相对路径解析(<prefix>/share/gqy/memes)。
    (share/"gqy").install "memes"
  end

  test do
    assert_match "gqy", shell_output("#{bin}/gqy --version 2>&1", 0)
  end
end
